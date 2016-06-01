# dirstate.py - sqlite backed dirstate
#
# Copyright 2016 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.
"""
dirstate class replacement with sqlite backed storage

This allows us to make incremental changes to we don't have to read the whole
dirstate on every operation.  This makes sense only when fsmonitor is on so we
don't iterate through whole dirstate.

sqldirstate stores the data in unnormalized form to avoid reading whole dirstate
to generate data like flat dirstate does.

It'is using the sqlite transactions to handle dirstate transactions instead of
copying db around.  As a result of that "hg rollback" doesn't work anymore. You
can fall back to copying things by setting sqldirstate.skipbackups to False.

We commit sql transaction only when normal dirstate write would happen.
"""

import os
import sqlite3

from mercurial import dirstate, parsers, util

from mercurial.node import nullid, hex, bin
from mercurial.util import propertycache

from sqlmap import sqlmap

dirstatetuple = parsers.dirstatetuple

DBFILE = "dirstate.sqlite3"
FAKEDIRSTATE = "dirstate"
SQLITE_CACHE_SIZE = -100000 # 100MB

def createotherschema(sqlconn):
    """ The storage for all misc small key value data """
    sqlconn.execute('''CREATE TABLE IF NOT EXISTS other (
                    key BLOB PRIMARY KEY,
                    value BLOB NOT NULL)
    ''')
    sqlconn.commit()
    cur = sqlconn.cursor()
    cur.execute('''SELECT key, value FROM other
                    WHERE key = "schema_version"''')
    row = cur.fetchone()
    if row is None:
        sqlconn.execute('''INSERT OR REPLACE INTO other (key, value) VALUES
                    ("schema_version", 1)''')
        sqlconn.commit()

def dropotherschema(sqlconn):
    cur = sqlconn.cursor()
    cur.execute('''DROP TABLE IF EXISTS other''')
    cur.close()
    sqlconn.commit()

class sqldirstatemap(sqlmap):
    """ the main map - reflects the original dirstate file contents """
    _tablename = 'files'
    _keyname = 'filename'
    _valuenames = ['status', 'mode', 'size', 'mtime']

    def createschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''CREATE TABLE IF NOT EXISTS files (
                        filename BLOB PRIMARY KEY,
                        status BLOB NOT NULL,
                        mode INTEGER NOT NULL,
                        size INTEGER NOT NULL,
                        mtime INTEGER NOT NULL)
        ''')
        cur.execute('''CREATE INDEX IF NOT EXISTS
                    files_mtime ON files(mtime);''')

        cur.close()
        self._sqlconn.commit()

    def dropschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''DROP TABLE IF EXISTS files''')
        cur.close()
        self._sqlconn.commit()

    def _rowtovalue(self, row):
        return dirstatetuple(*row)

    def _valuetorow(self, value):
        return (value[0], value[1], value[2], value[3])

    def nonnormalentries(self):
        cur = self._sqlconn.cursor()
        # -1 means that we should check the file on next status
        cur.execute('''SELECT filename FROM files
                    WHERE status != 'n' or mtime = -1''')
        rows = cur.fetchall()
        cur.close()
        return set(row[0] for row in rows)

    def otherparententries(self):
        cur = self._sqlconn.cursor()
        # -2 means that file is comming from the other parent of the merge
        # it's always dirty
        cur.execute('''SELECT filename, status, mode, size, mtime FROM files '''
                    '''WHERE status = 'n' and size = -2;''')
        for r in cur:
            yield (r[0], self._rowtovalue(r[1:]))
        cur.close()

    def modifiedentries(self):
        cur = self._sqlconn.cursor()
        cur.execute('''SELECT filename, status, mode, size, mtime FROM files '''
                    '''WHERE status = 'm';''')
        for r in cur:
            yield (r[0], self._rowtovalue(r[1:]))
        cur.close()

    def resetnow(self, now):
        cur = self._sqlconn.cursor()
        cur.execute('''UPDATE files SET mtime = -1
                    WHERE status = 'n' and mtime = ?''', (now,))
        cur.close()

class sqlcopymap(sqlmap):
    """ all copy informations in dirstate """
    _tablename = 'copymap'
    _keyname = 'dest'
    _valuenames = ['source']
    def createschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''CREATE TABLE IF NOT EXISTS  copymap(
                        dest BLOB PRIMARY KEY,
                        source BLOB NOT NULL)
        ''')
        cur.close()
        self._sqlconn.commit()

    def dropschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''DROP TABLE IF EXISTS copymap''')
        cur.close()
        self._sqlconn.commit()


class sqlfilefoldmap(sqlmap):
    """ in normal dirstate this map is generated on-the-fly from
    the dirstate. We are opting for persistent foldmap so we don't
    have read the whole dirstate """

    _tablename = 'filefoldmap'
    _keyname = 'normed'
    _valuenames = ['real']
    def createschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''CREATE TABLE IF NOT EXISTS filefoldmap (
                        normed BLOB PRIMARY KEY,
                        real BLOB NOT NULL)
        ''')
        cur.close()
        self._sqlconn.commit()

    def dropschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''DROP TABLE IF EXISTS filefoldmap''')
        cur.close()
        self._sqlconn.commit()

class sqldirfoldmap(sqlmap):
    """ in normal dirstate this map is generated on-the-fly from
    the dirstate. We are opting for persistent foldmap so we don't
    have read the whole dirstate """

    _tablename = 'dirfoldmap'
    _keyname = 'normed'
    _valuenames = ['real']
    def createschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''CREATE TABLE IF NOT EXISTS dirfoldmap (
                        normed BLOB PRIMARY KEY,
                        real BLOB NOT NULL)
        ''')
        cur.close()
        self._sqlconn.commit()

    def dropschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''DROP TABLE IF EXISTS dirfoldmap''')
        cur.close()
        self._sqlconn.commit()


class sqldirsdict(sqlmap):
    """ in normal dirstate this map is generated on-the-fly from
    the dirstate. We are opting for persistent foldmap so we don't
    have read the whole dirstate """

    _tablename = 'dirs'
    _keyname = 'dir'
    _valuenames = ['count']
    def createschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''CREATE TABLE IF NOT EXISTS dirs(
                        dir BLOB PRIMARY KEY,
                        count INT NOT NULL)
        ''')
        cur.close()
        self._sqlconn.commit()

    def dropschema(self):
        cur = self._sqlconn.cursor()
        cur.execute('''DROP TABLE IF EXISTS dirs''')
        cur.close()
        self._sqlconn.commit()

class sqldirs(object):
    """ Reimplementaion of util.dirs which is not resuseable because it's
        replaced by c code if available. Probably with a small upstream
        change we could reuse it """
    def __init__(self, sqlconn, skip=None, filemap=None, dirsdict=None):
        self._dirs = dirsdict
        if self._dirs is None:
            self._dirs = sqldirsdict(sqlconn)
        if filemap:
            for f, s in filemap.iteritems():
                self.addpath(f)

    # copied from util.py
    def addpath(self, path):
        dirs = self._dirs
        for base in util.finddirs(path):
            if base in dirs:
                dirs[base] += 1
                return
            dirs[base] = 1

    def delpath(self, path):
        dirs = self._dirs
        for base in util.finddirs(path):
            if dirs[base] > 1:
                dirs[base] -= 1
                return
            del dirs[base]

    def __iter__(self):
        return self._dirs.iterkeys()

    def __contains__(self, d):
        return d in self._dirs

    def clear(self):
        self._dirs.clear()

    @property
    def dirsdict(self):
        return self._dirs

def makedirstate(cls):
    class sqldirstate(cls):
        def _sqlinit(self):
            '''Create a new dirstate object.

            opener is an open()-like callable that can be used to open the
            dirstate file; root is the root of the directory tracked by
            the dirstate.
            '''
            self._sqlfilename = self._opener.join(DBFILE)
            self._sqlconn = sqlite3.connect(self._sqlfilename)
            self._sqlconn.text_factory = str
            self._sqlconn.execute("PRAGMA cache_size = %d" % SQLITE_CACHE_SIZE)
            createotherschema(self._sqlconn)
            self._map = sqldirstatemap(self._sqlconn)
            self._dirs = sqldirs(self._sqlconn)
            self._copymap = sqlcopymap(self._sqlconn)
            self._filefoldmap = sqlfilefoldmap(self._sqlconn)
            self._dirfoldmap = sqldirfoldmap(self._sqlconn)
            self.skipbackups = self._ui.configbool('sqldirstate', 'skipbackups',
                                                   True)

        def _read(self):
            pass

        @propertycache
        def _pl(self):
            p1 = p2 = hex(nullid)
            cur = self._sqlconn.cursor()
            cur.execute('''SELECT key, value FROM other
                        WHERE key='p1' or key='p2' ''')
            rows = cur.fetchall()
            for r in rows:
                if r[0] == 'p1':
                    p1 = r[1]
                if r[0] == 'p2':
                    p2 = r[1]

            cur.close()
            return [bin(p1), bin(p2)]

        def __setattr__(self, key, value):
            if key == '_pl':
                # because other methods in dirstate are setting it directly
                # instead of using setparents
                p1 = value[0]
                p2 = value[1]
                cur = self._sqlconn.cursor()
                cur.executemany('''INSERT OR REPLACE INTO
                            other (key, value) VALUES (?, ?)''',
                            [('p1', hex(p1)), ('p2', hex(p2))])
                cur.close()
                self.__dict__['_pl'] = value
            else:
                return super(sqldirstate, self).__setattr__(key, value)

        def savebackup(self, tr, suffix="", prefix=""):
            if self.skipbackups:
                return
            self._writesqldirstate()
            util.copyfile(self._opener.join(DBFILE),
                          self._opener.join(prefix + DBFILE + suffix))

        def restorebackup(self, tr, suffix="", prefix=""):
            if self.skipbackups:
                return
            self._opener.rename(prefix + DBFILE + suffix, DBFILE)
            self.invalidate()

        def clearbackup(self, tr, suffix="", prefix=""):
            if self.skipbackups:
                return
            self._opener.unlink(prefix + DBFILE + suffix)

        @propertycache
        def _nonnormalset(self):
            return self._map.nonnormalentries()

        def invalidate(self):
            # Transaction will be rolled back on the next open of the file.
            # The close is faster in the case where we replace  the whole
            # db file as we don't need to rollback in such case.
            self._sqlconn.close()
            for a in ("_branch", "_pl", "_ignore", "_nonnormalset"):
                if a in self.__dict__:
                    delattr(self, a)
            self._lastnormaltime = 0
            self._dirty = False
            self._parentwriters = 0
            self._sqlinit()

        def write(self, tr=False):
            # if dirty dump to disk (db transaction commit)
            if not self._dirty:
                return
            now = dirstate._getfsnow(self._opener)

            self._map.resetnow(now)
            if tr:
                tr.addfinalize("sqldirstate.write", self._backupandwrite)
                return
            self._writesqldirstate()

        def _writedirstate(self, st):
            self._writesqldirstate()

        def _writesqldirstate(self):
            # if dirty dump to disk (db transaction commit)
            now = dirstate._getfsnow(self._opener)

            self._map.resetnow(now)
            self._sqlconn.commit()
            self._lastnormaltime = 0
            self._dirty = self._dirtypl = False
            self._nonnormalset = self._map.nonnormalentries()

            if self._ui.configbool('sqldirstate', 'fakedirstate', True):
                writefakedirstate(self)

        def _backupandwrite(self, tr):
            if not self.skipbackups:
                backuppath = self._opener.join('%s.%s' % (tr.journal, DBFILE))
                util.copyfile(self._sqlfilename, backuppath)
                tr._addbackupentry(('plain', self._sqlfilename,
                                    backuppath, False))
            self._writesqldirstate()

        def clear(self):
            self._map.clear()
            self._nonnormalset = set()
            self._dirs.clear()
            self._copymap.clear()
            self._filefoldmap.clear()
            self._dirfoldmap.clear()
            self._pl = [nullid, nullid]
            self._lastnormaltime = 0
            self._dirty = True

        def setparents(self, p1, p2=nullid):
            """Set dirstate parents to p1 and p2.

            When moving from two parents to one, 'm' merged entries a
            adjusted to normal and previous copy records discarded and
            returned by the call.

            See localrepo.setparents()
            """
            if self._parentwriters == 0:
                raise ValueError("cannot set dirstate parent without "
                                "calling dirstate.beginparentchange")

            self._dirty = self._dirtypl = True
            oldp2 = self._pl[1]
            self._pl = p1, p2
            copies = {}
            if oldp2 != nullid and p2 == nullid:
                # Discard 'm' markers when moving away from a merge state
                for f, s in self._map.modifiedentries():
                    if f in self._copymap:
                        copies[f] = self._copymap[f]
                    self.normallookup(f)
                # Also fix up otherparent markers
                for f, s in self._map.otherparententries():
                    if f in self._copymap:
                        copies[f] = self._copymap[f]
                    self.add(f)
            return copies
    return sqldirstate


def writefakedirstate(dirstate):
    st = dirstate._opener(FAKEDIRSTATE, "w", atomictemp=True)
    st.write("".join(dirstate._pl))
    st.write("\nThis is fake dirstate put here by the sqldirsate.")
    st.write("\nIt contains only working copy parents info.")
    st.write("\nThe real dirstate is in dirstate.sqlite3 file.")
    st.close()

def tosql(dirstate):
    """" converts flat dirstate to sqldirstate

    note: the sole responsibility of this function is to write the new dirstate
    it's not touching anything but the DBFILE
    """
    sqlfilename = dirstate._opener.join(DBFILE)
    try:
        os.unlink(sqlfilename)
    except OSError:
        pass

    sqlconn = sqlite3.connect(sqlfilename)

    sqlconn.text_factory = str

    # Those two pragmas are dangerous and may corrupt db if interrupted but we
    # are populating db now so we are not afraid to lose it and we want it to
    # be fast.
    sqlconn.execute("PRAGMA synchronous = OFF")
    sqlconn.execute("PRAGMA journal_mode = OFF")
    sqlconn.execute("PRAGMA cache_size = %d" % SQLITE_CACHE_SIZE)

    createotherschema(sqlconn)
    sqlmap = sqldirstatemap(sqlconn)
    copymap = sqlcopymap(sqlconn)
    filefoldmap = sqlfilefoldmap(sqlconn)
    dirfoldmap = sqldirfoldmap(sqlconn)
    dirsdict = sqldirsdict(sqlconn)

    sqldirs(sqlconn, dirsdict=dirstate._map)
    sqlmap.update(dirstate._map)
    copymap.update(dirstate._copymap)
    filefoldmap.update(dirstate._filefoldmap)
    dirfoldmap.update(dirstate._dirfoldmap)
    dirs = sqldirs(sqlconn, filemap=dirstate._map, dirsdict={})
    dirsdict.update(dirs.dirsdict)

    cur = sqlconn.cursor()
    cur.executemany('''INSERT OR REPLACE INTO
                other (key, value)
                VALUES (?, ?)''',
                [('p1', hex(dirstate.p1())), ('p2', hex(dirstate.p2()))]
                )
    cur.close()
    sqlconn.commit()
    writefakedirstate(dirstate)

    sqlconn.execute("PRAGMA synchronous = ON")
    sqlconn.execute("PRAGMA journal_mode = DELETE")

def toflat(sqldirstate):
    """" converts sqldirstate to flat dirstate

    note: the sole responsibility of this function is to write the new dirstate
    it's not touching anything but the dirstate file
    """
    # converts a sqldirstate to a flat one
    st = sqldirstate._opener("dirstate", "w", atomictemp=True)
    newmap = {}
    for k, v in sqldirstate._map.iteritems():
        newmap[k] = v
    newcopymap = {}
    for k, v in sqldirstate._copymap.iteritems():
        newcopymap[k] = v

    st.write(parsers.pack_dirstate(newmap, newcopymap, sqldirstate._pl,
                                   dirstate._getfsnow(sqldirstate._opener)))
    st.close()
