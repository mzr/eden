/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/inodes/Traverse.h"

#include <folly/logging/xlog.h>
#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/TreeInode.h"

namespace facebook::eden {

namespace {

std::vector<ChildEntry> parseDirContents(const DirContents& contents) {
  std::vector<ChildEntry> results;
  results.reserve(contents.size());
  for (const auto& [name, entry] : contents) {
    results.push_back(ChildEntry{
        name,
        entry.getDtype(),
        entry.getInodeNumber(),
        entry.getOptionalHash(),
        entry.getInodePtr()});
  }
  return results;
}

} // namespace

void traverseTreeInodeChildren(
    Overlay* overlay,
    const std::vector<ChildEntry>& children,
    RelativePathPiece rootPath,
    InodeNumber ino,
    const std::optional<ObjectId>& hash,
    uint64_t fsRefcount,
    TraversalCallbacks& callbacks) {
  callbacks.visitTreeInode(rootPath, ino, hash, fsRefcount, children);
  for (auto& entry : children) {
    auto childPath = rootPath + entry.name;
    if (auto child = entry.loadedChild) {
      if (auto* loadedTreeInode = child.asTreeOrNull()) {
        if (callbacks.shouldRecurse(entry)) {
          traverseObservedInodes(*loadedTreeInode, childPath, callbacks);
        }
      }
    } else {
      if (dtype_t::Dir == entry.dtype) {
        if (callbacks.shouldRecurse(entry)) {
          // If we are able to load a child directory from the overlay, then
          // this child entry has been allocated, and can be traversed.
          auto contents = overlay->loadOverlayDir(ino);
          if (!contents.empty()) {
            traverseTreeInodeChildren(
                overlay,
                parseDirContents(contents),
                childPath,
                entry.ino,
                entry.hash,
                0,
                callbacks);
          }
        }
      }
    }
  }
}

void traverseObservedInodes(
    const TreeInode& root,
    RelativePathPiece rootPath,
    TraversalCallbacks& callbacks) {
  auto* overlay = root.getMount()->getOverlay();

  std::vector<ChildEntry> children;
  std::optional<ObjectId> hash;
  {
    auto contents = root.getContents().rlock();
    children = parseDirContents(contents->entries);
    hash = contents->treeHash;
  }

  traverseTreeInodeChildren(
      overlay,
      children,
      rootPath,
      root.getNodeId(),
      hash,
      root.debugGetFsRefcount(),
      callbacks);
}

} // namespace facebook::eden
