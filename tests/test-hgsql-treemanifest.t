#testcases case-innodb case-rocksdb

#if case-rocksdb
  $ DBENGINE=rocksdb
#else
  $ DBENGINE=innodb
#endif

  $ . $TESTDIR/require-ext.sh treemanifest

  $ CACHEDIR=`pwd`/hgcache
  $ . "$TESTDIR/hgsql/library.sh"

  $ cat >> $HGRCPATH <<EOF
  > [extensions]
  > pushrebase=
  > EOF

Test that treemanifest backfill populates the database

  $ initserver master master
  $ initserver master-alreadysynced master
  $ initserver master-new master
  $ cd master
  $ touch a && hg ci -Aqm a
  $ mkdir dir
  $ touch dir/b && hg ci -Aqm b
  $ hg book master

  $ cd ../master-alreadysynced
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > treemanifest=
  > [treemanifest]
  > server = True
  > EOF
  $ hg log -r tip --forcesync -T '{rev}\n'
  1

  $ cd ../master
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > treemanifest=
  > [treemanifest]
  > server = True
  > EOF
  $ DBGD=1 hg backfilltree
  $ ls .hg/store/meta/dir
  00manifest.i

Test that an empty repo syncs the tree revlogs

  $ cd ../master-new
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > treemanifest=
  > [treemanifest]
  > server = True
  > EOF
  $ hg log -r tip --forcesync -T '{rev}\n'
  1
  $ ls .hg/store/meta/dir
  00manifest.i

Test that we can replay backfills into an existing repo
  $ cd ../master-alreadysynced
  $ hg sqlreplay
  $ ls .hg/store/meta/dir
  00manifest.i
  $ rm -rf .hg/store/00manifesttree* .hg/store/meta
  $ hg sqlreplay --start 0 --end 0
  $ hg debugindex .hg/store/00manifesttree.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
  $ hg sqlreplay --start 1 --end 2
  $ hg debugindex .hg/store/00manifesttree.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
       1        44      58      0       1 898d94054864 8515d4bfda76 000000000000
  $ cd ..

Test that trees created during push are synced to the db

  $ initclient client
  $ cd client
  $ hg pull -q ssh://user@dummy/master
  $ hg up -q tip
  $ touch dir/c && hg ci -Aqm c

  $ hg push ssh://user@dummy/master --to master
  pushing to ssh://user@dummy/master
  searching for changes
  remote: pushing 1 changeset:
  remote:     c46827e4453c  c

  $ cd ../master-new
  $ hg log -G -T '{rev} {desc}' --forcesync
  o  2 c
  |
  o  1 b
  |
  o  0 a
  
  $ hg debugdata .hg/store/meta/dir/00manifest.i 1
  b\x00b80de5d138758541c5f05265ad144ab9fa86d1db (esc)
  c\x00b80de5d138758541c5f05265ad144ab9fa86d1db (esc)

Test that sqltreestrip deletes trees from history
  $ cd ../client
  $ mkdir dir2
  $ echo >> dir2/d && hg ci -Aqm d
  $ echo >> dir2/d && hg ci -Aqm d2
  $ hg push ssh://user@dummy/master --to master
  pushing to ssh://user@dummy/master
  searching for changes
  remote: pushing 2 changesets:
  remote:     b3adfc03d09d  d
  remote:     fc50e1c24ca2  d2

  $ cd ../master
  $ hg log -G -T '{rev} {desc}' --forcesync
  o  4 d2
  |
  o  3 d
  |
  o  2 c
  |
  @  1 b
  |
  o  0 a
  
  $ hg sqltreestrip 2 --i-know-what-i-am-doing
  *** YOU ARE ABOUT TO DELETE TREE HISTORY INCLUDING AND AFTER 2 (MANDATORY 5 SECOND WAIT) ***
  mysql: deleting trees with linkrevs >= 2
  local: deleting trees with linkrevs >= 2
  $ hg debugindex .hg/store/00manifesttree.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
       1        44      58      0       1 898d94054864 8515d4bfda76 000000000000
  $ hg debugindex .hg/store/00manifest.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
       1        44      59      0       1 898d94054864 8515d4bfda76 000000000000
       2       103      59      1       2 7cdc42a14359 898d94054864 000000000000
       3       162      60      2       3 0c96405fb5c3 7cdc42a14359 000000000000
       4       222      60      3       4 8b833dfa4cc5 0c96405fb5c3 000000000000
  $ hg status --change 4 --config treemanifest.treeonly=True
  abort: "unable to find the following nodes locally or on the server: ('', 8b833dfa4cc566bfd4bcb4d85e4a128be5e49334)"
  [255]

Test local only strip
  $ cd ../master-alreadysynced
  $ hg sqltreestrip 2 --local-only --i-know-what-i-am-doing
  *** YOU ARE ABOUT TO DELETE TREE HISTORY INCLUDING AND AFTER 2 (MANDATORY 5 SECOND WAIT) ***
  local: deleting trees with linkrevs >= 2
  $ hg debugindex .hg/store/00manifesttree.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
       1        44      58      0       1 898d94054864 8515d4bfda76 000000000000

Refill trees in sql
(glob in the debugindex is because of different compression behavior in
different environments)
  $ cd ../master
  $ hg backfilltree
  $ hg debugindex .hg/store/00manifesttree.i
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0      44     -1       0 8515d4bfda76 000000000000 000000000000
       1        44      58      0       1 898d94054864 8515d4bfda76 000000000000
       2       102      58      1       2 7cdc42a14359 898d94054864 000000000000
       3       160      59      2       3 0c96405fb5c3 7cdc42a14359 000000000000
       4       219     *     -1       4 8b833dfa4cc5 0c96405fb5c3 000000000000 (glob)
  $ hg status --change 4 --config treemanifest.treeonly=True
  M dir2/d

Refill trees in the other master
  $ cd ../master-alreadysynced
  $ hg sqlreplay 2
  $ hg status --change 4 --config treemanifest.treeonly=True
  M dir2/d
