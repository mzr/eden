// Copyright Facebook, Inc. 2017
//! Directory State Tree.

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use errors::*;
use std::collections::Bound;
use std::io::{Cursor, Read, Write};
use store::{BlockId, Store, StoreView};
use vecmap::VecMap;

/// Trait that must be implemented by types that can be stored as the value in the tree.
pub trait Storable
where
    Self: Sized,
{
    /// Serialize the storable data to a `Write` stream.
    fn write(&self, w: &mut Write) -> Result<()>;

    /// Deserialize a new data item from a `Read` stream.
    fn read(r: &mut Read) -> Result<Self>;
}

/// A node entry is an entry in a directory, either a file or another directory.
#[derive(Debug)]
enum NodeEntry<T> {
    Directory(Node<T>),
    File(T),
}

/// Filenames are buffers of bytes.  They're not stored in Strings as they may not be UTF-8.
pub type Key = Vec<u8>;
pub type KeyRef<'a> = &'a [u8];

/// Store the node entries in an ordered map from name to node entry.
type NodeEntryMap<T> = VecMap<Key, NodeEntry<T>>;

/// The contents of a directory.
#[derive(Debug)]
struct Node<T> {
    /// The ID of the directory in the store.  If None, this directory has not yet been
    /// written to the back-end store in its current state.
    id: Option<BlockId>,

    /// The set of files and directories in this directory, indexed by their name.  If None,
    /// then the ID must not be None, and the entries are yet to be loaded from the back-end
    /// store.
    entries: Option<NodeEntryMap<T>>,
}

/// The root of the tree.  The count of files in the tree is maintained for fast size
/// determination.
pub struct Tree<T> {
    root: Node<T>,
    file_count: u32,
}

/// Utility enum for recursing through trees.
enum PathRecurse<'name, 'node, T: 'node> {
    Directory(KeyRef<'name>, KeyRef<'name>, &'node mut Node<T>),
    ExactDirectory(KeyRef<'name>, &'node mut Node<T>),
    MissingDirectory(KeyRef<'name>, KeyRef<'name>),
    File(KeyRef<'name>, &'node mut T),
    MissingFile(KeyRef<'name>),
    ConflictingFile(KeyRef<'name>, KeyRef<'name>, &'node mut T),
}

/// Splits a key into the first path element and the remaining path elements (if any).
fn split_key<'a>(key: KeyRef<'a>) -> (KeyRef<'a>, Option<KeyRef<'a>>) {
    // Skip the last character.  Even if it's a '/' we don't want to split on it.
    for (index, value) in key.iter().take(key.len() - 1).enumerate() {
        if *value == b'/' {
            return (&key[..index + 1], Some(&key[index + 1..]));
        }
    }
    (key, None)
}

impl<T: Storable + Clone> NodeEntry<T> {
    /// Read an entry from the store.  Returns the name and the entry.
    fn read(r: &mut Read) -> Result<(Key, NodeEntry<T>)> {
        let entry_type = r.read_u8()?;
        match entry_type {
            b'f' => {
                // File entry.
                let data = T::read(r)?;
                let name_len = r.read_u32::<BigEndian>()?;
                let mut name = Vec::with_capacity(name_len as usize);
                unsafe {
                    // Safe as we've just allocated the buffer and are about to read into it.
                    name.set_len(name_len as usize);
                }
                r.read_exact(name.as_mut_slice())?;
                Ok((name, NodeEntry::File(data)))
            }
            b'd' => {
                // Directory entry.
                let id = r.read_u64::<BigEndian>()?;
                let name_len = r.read_u32::<BigEndian>()?;
                let mut name = Vec::with_capacity(name_len as usize);
                unsafe {
                    // Safe as we've just allocated the buffer and are about to read into it.
                    name.set_len(name_len as usize);
                }
                r.read_exact(name.as_mut_slice())?;
                Ok((name, NodeEntry::Directory(Node::open(BlockId(id)))))
            }
            _ => {
                bail!(ErrorKind::CorruptTree);
            }
        }
    }
}

impl<T: Storable + Clone> Node<T> {
    /// Create a new empty Node.  This has no ID as it is not yet written to the store.
    fn new() -> Node<T> {
        Node {
            id: None,
            entries: Some(NodeEntryMap::new()),
        }
    }

    /// Create a new Node for an existing entry in the store.  The entries are not loaded until
    /// the load method is called.
    fn open(id: BlockId) -> Node<T> {
        Node {
            id: Some(id),
            entries: None,
        }
    }

    /// Attempt to load a node from a store.
    fn load(&mut self, store: &StoreView) -> Result<()> {
        if self.entries.is_some() {
            // Already loaded.
            return Ok(());
        }
        let id = self.id.expect("Node must have a valid ID to be loaded");
        let data = store.read(id)?;
        let len = data.len() as u64;
        let mut cursor = Cursor::new(data);
        let count = cursor.read_u32::<BigEndian>()? as usize;
        let mut entries = NodeEntryMap::with_capacity(count);
        while cursor.position() < len {
            let (name, entry) = NodeEntry::read(&mut cursor)?;
            entries.insert_hint_end(name, entry);
        }
        self.entries = Some(entries);
        Ok(())
    }

    /// Get access to the node entries, ensuring they are loaded first.
    #[inline]
    fn load_entries(&mut self, store: &StoreView) -> Result<&mut NodeEntryMap<T>> {
        self.load(store)?;
        let entries = self.entries
            .as_mut()
            .expect("Entries should have been populated by loading");
        Ok(entries)
    }

    /// Writes all entries for this node to the store.  Any child directory entries must have
    /// had IDs assigned to them.
    fn write_entries(&mut self, store: &mut Store) -> Result<()> {
        let mut data = Vec::new();
        let entries = self.entries
            .as_mut()
            .expect("Node should have entries populated before writing out.");
        data.write_u32::<BigEndian>(entries.len() as u32)?;
        for (name, entry) in entries.iter_mut() {
            match entry {
                &mut NodeEntry::File(ref file) => {
                    data.write_u8(b'f')?;
                    file.write(&mut data)?;
                }
                &mut NodeEntry::Directory(ref mut node) => {
                    data.write_u8(b'd')?;
                    data.write_u64::<BigEndian>(node.id.unwrap().0)?;
                }
            }
            data.write_u32::<BigEndian>(name.len() as u32)?;
            data.write(name)?;
        }
        self.id = Some(store.append(&data)?);
        Ok(())
    }

    /// Perform a full write of the node and its children to the store.  Old entries are
    /// loaded from the old_store before being written back to the new store.
    fn write_full(&mut self, store: &mut Store, old_store: &StoreView) -> Result<()> {
        // Write out all the child nodes.
        for (_name, entry) in self.load_entries(old_store)?.iter_mut() {
            if let &mut NodeEntry::Directory(ref mut node) = entry {
                node.write_full(store, old_store)?;
            }
        }
        // Write out this node.
        self.write_entries(store)
    }

    /// Perform a delta write of the node and its children to the store.  Entries that are
    /// already in the store will not be written again.
    fn write_delta(&mut self, store: &mut Store) -> Result<()> {
        if self.id.is_none() {
            // This node has been modified, write out a new copy of any children who have
            // also changed.  The entries list must already have been populated when the node
            // was modified, so no need to load it here.
            {
                let entries = self.entries
                    .as_mut()
                    .expect("Node should have entries populated if it was modified.");
                for (_name, entry) in entries.iter_mut() {
                    if let &mut NodeEntry::Directory(ref mut node) = entry {
                        node.write_delta(store)?;
                    }
                }
            }

            // Write out this node.
            self.write_entries(store)
        } else {
            // This node and its descendents have not been modified.
            Ok(())
        }
    }

    // Visit all of the files in under this node, by calling the visitor function on each one.
    fn visit<'a, F>(
        &'a mut self,
        store: &StoreView,
        path: &mut Vec<KeyRef<'a>>,
        visitor: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&Vec<KeyRef>, &T) -> Result<()>,
    {
        for (name, entry) in self.load_entries(store)?.iter_mut() {
            path.push(name);
            match entry {
                &mut NodeEntry::Directory(ref mut node) => {
                    node.visit(store, path, visitor)?;
                }
                &mut NodeEntry::File(ref file) => {
                    visitor(path, file)?;
                }
            }
            path.pop();
        }
        Ok(())
    }

    /// Get the first file in the subtree under this node.  If the subtree is not empty, returns a
    /// pair containing the path to the file as a reversed vector of key references for each path
    /// element, and a reference to the file.
    fn get_first<'node>(
        &'node mut self,
        store: &StoreView,
    ) -> Result<Option<(Vec<KeyRef<'node>>, &'node T)>> {
        for (name, entry) in self.load_entries(store)?.iter_mut() {
            match entry {
                &mut NodeEntry::Directory(ref mut node) => {
                    if let Some((mut next_name, next_file)) = node.get_first(store)? {
                        next_name.push(name);
                        return Ok(Some((next_name, next_file)));
                    }
                }
                &mut NodeEntry::File(ref file) => {
                    return Ok(Some((vec![name], file)));
                }
            }
        }
        Ok(None)
    }

    /// Get the next file after a particular file in the tree.  Returns a pair containing the path
    /// to the file as a reversed vector of key references for each path element, and a reference
    /// to the file, or None if there are no more files.
    fn get_next<'node>(
        &'node mut self,
        store: &StoreView,
        name: KeyRef,
    ) -> Result<Option<(Vec<KeyRef<'node>>, &'node T)>> {
        // Find the entry within this list, and what the remainder of the path is.
        let (elem, mut path) = split_key(name);

        // Get the next entry after the current one.  We need to look inside directories as we go.
        // The subpath we obtained from split_key is only relevant if we are looking inside the
        // directory the path refers to.
        for (entry_name, entry) in self.load_entries(store)?
            .range_mut((Bound::Included(elem), Bound::Unbounded))
        {
            match entry {
                &mut NodeEntry::Directory(ref mut node) => {
                    // The entry is a directory, check inside it.
                    if elem != entry_name.as_slice() {
                        // This directory is not the one we were initially looking inside.  We
                        // have moved on past that directory, so the rest of the path is no
                        // longer relevant.
                        path = None
                    }
                    let next = if let Some(path) = path {
                        // Find the next file after the given subpath.
                        node.get_next(store, path)?
                    } else {
                        // Find the first file in this subtree.
                        node.get_first(store)?
                    };
                    if let Some((mut next_name, next_file)) = next {
                        next_name.push(entry_name);
                        return Ok(Some((next_name, next_file)));
                    }
                }
                &mut NodeEntry::File(ref file) => {
                    // This entry is a file.  Skip over it if it is the original file.
                    if elem != entry_name.as_slice() {
                        return Ok(Some((vec![entry_name], file)));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Utility function for recursing through subdirectories.  Returns the appropriate
    /// PathRecurse variant for the current position in the file tree given by name.
    fn path_recurse<'name, 'node>(
        &'node mut self,
        store: &StoreView,
        name: KeyRef<'name>,
    ) -> Result<PathRecurse<'name, 'node, T>> {
        let (elem, path) = split_key(name);
        let res = if let Some(path) = path {
            // The name is for a subdirectory.
            match self.load_entries(store)?.get_mut(elem) {
                Some(&mut NodeEntry::Directory(ref mut node)) => {
                    PathRecurse::Directory(elem, path, node)
                }
                Some(&mut NodeEntry::File(ref mut file)) => {
                    PathRecurse::ConflictingFile(elem, path, file)
                }
                None => PathRecurse::MissingDirectory(elem, path),
            }
        } else {
            // The name is for a file or directory in this directory.
            match self.load_entries(store)?.get_mut(elem) {
                Some(&mut NodeEntry::Directory(ref mut node)) => {
                    PathRecurse::ExactDirectory(elem, node)
                }
                Some(&mut NodeEntry::File(ref mut file)) => PathRecurse::File(elem, file),
                None => PathRecurse::MissingFile(elem),
            }
        };
        Ok(res)
    }

    /// Get a file's state.
    fn get<'node>(&'node mut self, store: &StoreView, name: KeyRef) -> Result<Option<&'node T>> {
        match self.path_recurse(store, name)? {
            PathRecurse::Directory(_dir, path, node) => node.get(store, path),
            PathRecurse::ExactDirectory(_dir, _node) => Ok(None),
            PathRecurse::MissingDirectory(_dir, _path) => Ok(None),
            PathRecurse::File(_name, file) => Ok(Some(file)),
            PathRecurse::MissingFile(_name) => Ok(None),
            PathRecurse::ConflictingFile(_name, _path, _file) => Ok(None),
        }
    }

    /// Returns true if the given path is a directory.
    fn has_dir(&mut self, store: &StoreView, name: KeyRef) -> Result<bool> {
        match self.path_recurse(store, name)? {
            PathRecurse::Directory(_dir, path, node) => node.has_dir(store, path),
            PathRecurse::ExactDirectory(_dir, _node) => Ok(true),
            PathRecurse::MissingDirectory(_dir, _path) => Ok(false),
            PathRecurse::File(_name, _file) => Ok(false),
            PathRecurse::MissingFile(_name) => Ok(false),
            PathRecurse::ConflictingFile(_name, _path, _file) => Ok(false),
        }
    }

    /// Add a file to the node.  The name may contain a path, in which case sufficient
    /// subdirectories are updated to add or update the file.
    fn add(&mut self, store: &StoreView, name: KeyRef, info: &T) -> Result<bool> {
        let (new_entry, file_added) = match self.path_recurse(store, name)? {
            PathRecurse::Directory(_dir, path, node) => {
                // The file is in a subdirectory.  Add it to the subdirectory.
                let file_added = node.add(store, path, info)?;
                (None, file_added)
            }
            PathRecurse::ExactDirectory(_dir, _node) => {
                panic!("Adding file which matches the name of a directory.");
            }
            PathRecurse::MissingDirectory(dir, path) => {
                // The file is in a new subdirectory.  Create the directory and add the file.
                let mut node = Node::new();
                let file_added = node.add(store, path, info)?;
                (Some((dir.to_vec(), NodeEntry::Directory(node))), file_added)
            }
            PathRecurse::File(_name, file) => {
                // The file is in this directory.  Update it.
                file.clone_from(info);
                (None, false)
            }
            PathRecurse::MissingFile(ref name) => {
                // The file should be in this directory.  Add it.
                (Some((name.to_vec(), NodeEntry::File(info.clone()))), true)
            }
            PathRecurse::ConflictingFile(_name, _path, _file) => {
                panic!("Adding file with path prefix that matches the name of a file.")
            }
        };
        if let Some((new_key, new_entry)) = new_entry {
            self.load_entries(store)?.insert(new_key, new_entry);
        }
        self.id = None;
        Ok(file_added)
    }

    /// Remove a file from the node.  The name may contain a path, in which case sufficient
    /// subdirectories are updated to remove the file.
    ///
    /// Returns a pair of booleans (file_removed, now_empty) indicating whether the file
    /// was removed, and whether the diectory is now empty.
    fn remove(&mut self, store: &StoreView, name: KeyRef) -> Result<(bool, bool)> {
        let (file_removed, remove_entry) = match self.path_recurse(store, name)? {
            PathRecurse::Directory(dir, path, node) => {
                let (file_removed, now_empty) = node.remove(store, path)?;
                (file_removed, if now_empty { Some(dir) } else { None })
            }
            PathRecurse::ExactDirectory(_dir, _node) => (false, None),
            PathRecurse::MissingDirectory(_dir, _path) => (false, None),
            PathRecurse::File(name, _file) => (true, Some(name)),
            PathRecurse::MissingFile(_name) => (false, None),
            PathRecurse::ConflictingFile(_name, _path, _file) => (false, None),
        };
        if let Some(entry) = remove_entry {
            self.load_entries(store)?.remove(entry);
            self.id = None;
        }
        if file_removed {
            self.id = None;
        }
        Ok((file_removed, self.load_entries(store)?.is_empty()))
    }
}

impl<T: Storable + Clone> Tree<T> {
    /// Create a new empty tree.
    pub fn new() -> Tree<T> {
        Tree {
            root: Node::new(),
            file_count: 0,
        }
    }

    /// Create a tree that references an existing root node.
    pub fn open(root_id: BlockId, file_count: u32) -> Tree<T> {
        Tree {
            root: Node::open(root_id),
            file_count,
        }
    }

    /// Clear all entries in the tree.
    pub fn clear(&mut self) {
        self.root = Node::new();
        self.file_count = 0;
    }

    pub fn root_id(&self) -> Option<BlockId> {
        self.root.id
    }

    pub fn file_count(&self) -> u32 {
        self.file_count
    }

    pub fn write_full(&mut self, store: &mut Store, old_store: &StoreView) -> Result<()> {
        self.root.write_full(store, old_store)?;
        Ok(())
    }

    pub fn write_delta(&mut self, store: &mut Store) -> Result<()> {
        self.root.write_delta(store)?;
        Ok(())
    }

    pub fn get<'a>(&'a mut self, store: &StoreView, name: KeyRef) -> Result<Option<&'a T>> {
        Ok(self.root.get(store, name)?)
    }

    pub fn visit<F>(&mut self, store: &StoreView, visitor: &mut F) -> Result<()>
    where
        F: FnMut(&Vec<KeyRef>, &T) -> Result<()>,
    {
        let mut path = Vec::new();
        self.root.visit(store, &mut path, visitor)
    }

    pub fn get_first<'a>(&'a mut self, store: &StoreView) -> Result<Option<(Key, &'a T)>> {
        Ok(self.root.get_first(store)?.map(|(mut path, file)| {
            path.reverse();
            (path.concat(), file)
        }))
    }

    pub fn get_next<'a>(
        &'a mut self,
        store: &StoreView,
        name: KeyRef,
    ) -> Result<Option<(Key, &'a T)>> {
        Ok(self.root.get_next(store, name)?.map(|(mut path, file)| {
            path.reverse();
            (path.concat(), file)
        }))
    }

    pub fn has_dir(&mut self, store: &StoreView, name: KeyRef) -> Result<bool> {
        Ok(self.root.has_dir(store, name)?)
    }

    pub fn add(&mut self, store: &StoreView, name: KeyRef, file: &T) -> Result<()> {
        if self.root.add(store, name, file)? {
            self.file_count += 1;
        }
        Ok(())
    }

    pub fn remove(&mut self, store: &StoreView, name: KeyRef) -> Result<bool> {
        let removed = self.root.remove(store, name)?.0;
        if removed {
            assert!(self.file_count > 0);
            self.file_count -= 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {

    use store::NullStore;
    use store::tests::MapStore;
    use tree::{KeyRef, Tree};
    use filestate::FileState;

    // Test files in order.  Note lexicographic ordering of file9 and file10.
    static TEST_FILES: [(&[u8], u32, i32, i32); 16] = [
        (b"dirA/subdira/file1", 0o644, 1, 10001),
        (b"dirA/subdira/file2", 0o644, 2, 10002),
        (b"dirA/subdirb/file3", 0o644, 3, 10003),
        (b"dirB/subdira/file4", 0o644, 4, 10004),
        (b"dirB/subdira/subsubdirx/file5", 0o644, 5, 10005),
        (b"dirB/subdira/subsubdiry/file6", 0o644, 6, 10006),
        (b"dirB/subdira/subsubdirz/file7", 0o755, 7, 10007),
        (b"dirB/subdira/subsubdirz/file8", 0o755, 8, 10008),
        (b"dirB/subdirb/file10", 0o644, 10, 10010),
        (b"dirB/subdirb/file9", 0o644, 9, 10009),
        (b"dirC/file11", 0o644, 11, 10011),
        (b"dirC/file12", 0o644, 12, 10012),
        (b"dirC/file13", 0o644, 13, 10013),
        (b"dirC/file14", 0o644, 14, 10014),
        (b"dirC/file15", 0o644, 15, 10015),
        (b"file16", 0o644, 16, 10016),
    ];

    fn populate(t: &mut Tree<FileState>, s: &MapStore) {
        for &(name, mode, size, mtime) in TEST_FILES.iter() {
            t.add(s, name, &FileState::new(b'n', mode, size, mtime))
                .expect("can add file");
        }
    }

    #[test]
    fn count_get_and_remove() {
        let ms = MapStore::new();
        let mut t = Tree::new();
        assert_eq!(t.file_count(), 0);
        assert_eq!(
            t.get(&ms, b"dirB/subdira/subsubdirz/file7")
                .expect("can get"),
            None
        );
        populate(&mut t, &ms);
        assert_eq!(t.file_count(), 16);
        assert_eq!(
            t.get(&ms, b"dirB/subdira/subsubdirz/file7")
                .expect("can get"),
            Some(&FileState::new(b'n', 0o755, 7, 10007))
        );
        t.remove(&ms, b"dirB/subdirb/file9").expect("can remove");
        assert_eq!(t.file_count(), 15);
        t.remove(&ms, b"dirB/subdirb/file10").expect("can remove");
        assert_eq!(t.file_count(), 14);
        assert_eq!(
            t.get(&ms, b"dirB/subdira/subsubdirz/file7")
                .expect("can get"),
            Some(&FileState::new(b'n', 0o755, 7, 10007))
        );
        assert_eq!(t.get(&ms, b"dirB/subdirb/file9").expect("can get"), None);
    }

    #[test]
    fn iterate() {
        let ms = MapStore::new();
        let mut t = Tree::new();
        assert_eq!(t.get_first(&ms).expect("can get first"), None);
        populate(&mut t, &ms);
        let mut expect_iter = TEST_FILES.iter();
        let expected = expect_iter.next().unwrap();
        let mut filename = expected.0.to_vec();
        assert_eq!(
            t.get_first(&ms).expect("can get first"),
            Some((
                filename.clone(),
                &FileState::new(b'n', expected.1, expected.2, expected.3)
            ))
        );
        while let Some(expected) = expect_iter.next() {
            let actual = t.get_next(&ms, &filename).expect("can get next");
            filename = expected.0.to_vec();
            assert_eq!(
                actual,
                Some((
                    filename.clone(),
                    &FileState::new(b'n', expected.1, expected.2, expected.3)
                ))
            );
        }
        assert_eq!(t.get_next(&ms, &filename).expect("can get next"), None);
    }

    #[test]
    fn has_dir() {
        let ms = MapStore::new();
        let mut t = Tree::new();
        assert_eq!(
            t.has_dir(&ms, b"anything/").expect("can check has_dir"),
            false
        );
        populate(&mut t, &ms);
        assert_eq!(
            t.has_dir(&ms, b"something else/")
                .expect("can check has_dir"),
            false
        );
        assert_eq!(t.has_dir(&ms, b"dirB/").expect("can check has_dir"), true);
        assert_eq!(
            t.has_dir(&ms, b"dirB/subdira/").expect("can check has_dir"),
            true
        );
        assert_eq!(
            t.has_dir(&ms, b"dirB/subdira/subsubdirz/")
                .expect("can check has_dir"),
            true
        );
        assert_eq!(
            t.has_dir(&ms, b"dirB/subdira/subsubdirz/file7")
                .expect("can check has_dir"),
            false
        );
        assert_eq!(
            t.has_dir(&ms, b"dirB/subdira/subsubdirz/file7/")
                .expect("can check has_dir"),
            false
        );
    }

    #[test]
    fn write_empty() {
        let ns = NullStore::new();
        let mut ms = MapStore::new();
        let mut t = Tree::<FileState>::new();
        t.write_full(&mut ms, &ns).expect("can write full");
        t.write_delta(&mut ms).expect("can write delta");
        let mut ms2 = MapStore::new();
        t.write_full(&mut ms2, &ms).expect("can write full");
        let t_root = t.root_id().unwrap();
        let t_count = t.file_count();
        let mut t2 = Tree::<FileState>::open(t_root, t_count);
        assert_eq!(t2.get_first(&ms2).expect("can get first"), None);
    }

    #[test]
    fn write() {
        let ns = NullStore::new();
        let mut ms = MapStore::new();
        let mut t = Tree::new();
        populate(&mut t, &ms);
        t.write_full(&mut ms, &ns).expect("can write full");
        t.write_delta(&mut ms).expect("can write delta");
        let mut ms2 = MapStore::new();
        t.write_full(&mut ms2, &ms).expect("can write full");
        let t_root = t.root_id().unwrap();
        let t_count = t.file_count();
        let mut t2 = Tree::open(t_root, t_count);
        assert_eq!(
            t2.get(&ms2, b"dirB/subdira/subsubdirz/file7")
                .expect("can get"),
            Some(&FileState::new(b'n', 0o755, 7, 10007))
        );
    }

    #[test]
    fn visit() {
        let mut ms = MapStore::new();
        let mut t = Tree::new();
        populate(&mut t, &ms);
        let mut files = Vec::new();
        {
            let mut v = |path: &Vec<KeyRef>, _fs: &FileState| {
                files.push(path.concat());
                Ok(())
            };
            t.visit(&mut ms, &mut v).expect("can visit");
        }
        assert_eq!(
            files,
            TEST_FILES
                .iter()
                .map(|t| t.0.to_vec())
                .collect::<Vec<Vec<u8>>>()
        );
    }
}
