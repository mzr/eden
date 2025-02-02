#chg-compatible


#require p4

  $ enable convert

create p4 depot
  $ P4ROOT=`pwd`/depot; export P4ROOT
  $ P4AUDIT=$P4ROOT/audit; export P4AUDIT
  $ P4JOURNAL=$P4ROOT/journal; export P4JOURNAL
  $ P4LOG=$P4ROOT/log; export P4LOG
  $ P4PORT=localhost:$HGPORT; export P4PORT
  $ P4DEBUG=1; export P4DEBUG

start the p4 server
  $ [ ! -d $P4ROOT ] && mkdir $P4ROOT
  $ p4d -f -J off >$P4ROOT/stdout 2>$P4ROOT/stderr &
  $ echo $! >> $DAEMON_PIDS
  $ trap "echo stopping the p4 server ; p4 admin stop" EXIT

  $ # wait for the server to initialize
  $ while ! p4 ; do
  >    sleep 1
  > done >/dev/null 2>/dev/null

create a client spec
  $ P4CLIENT=hg-p4-import; export P4CLIENT
  $ DEPOTPATH=//depot/test-mercurial-import/...
  $ p4 client -o | sed '/^View:/,$ d' >p4client
  $ echo View: >>p4client
  $ echo " $DEPOTPATH //$P4CLIENT/..." >>p4client
  $ p4 client -i <p4client
  Client hg-p4-import saved.

populate the depot
  $ echo a > a
  $ mkdir b
  $ echo c > b/c
  $ p4 add a b/c
  //depot/test-mercurial-import/a#1 - opened for add
  //depot/test-mercurial-import/b/c#1 - opened for add
  $ p4 submit -d initial
  Submitting change 1.
  Locking 2 files ...
  add //depot/test-mercurial-import/a#1
  add //depot/test-mercurial-import/b/c#1
  Change 1 submitted.

change some files
  $ p4 edit a
  //depot/test-mercurial-import/a#1 - opened for edit
  $ echo aa >> a
  $ p4 submit -d "change a"
  Submitting change 2.
  Locking 1 files ...
  edit //depot/test-mercurial-import/a#2
  Change 2 submitted.

  $ p4 edit b/c
  //depot/test-mercurial-import/b/c#1 - opened for edit
  $ echo cc >> b/c
  $ p4 submit -d "change b/c"
  Submitting change 3.
  Locking 1 files ...
  edit //depot/test-mercurial-import/b/c#2
  Change 3 submitted.

convert
  $ hg convert -s p4 $DEPOTPATH dst
  initializing destination dst repository
  scanning source...
  reading p4 views
  collecting p4 changelists
  1 initial
  2 change a
  3 change b/c
  sorting...
  converting...
  2 initial
  1 change a
  0 change b/c
  $ hg -R dst log --template 'desc="{desc}" tags="{tags}" files="{files}"\n'
  desc="change b/c" tags="" files="b/c"
  desc="change a" tags="" files="a"
  desc="initial" tags="" files="a b/c"

change some files
  $ p4 edit a b/c
  //depot/test-mercurial-import/a#2 - opened for edit
  //depot/test-mercurial-import/b/c#2 - opened for edit
  $ echo aaa >> a
  $ echo ccc >> b/c
  $ p4 submit -d "change a b/c"
  Submitting change 4.
  Locking 2 files ...
  edit //depot/test-mercurial-import/a#3
  edit //depot/test-mercurial-import/b/c#3
  Change 4 submitted.

convert again
  $ hg convert -s p4 $DEPOTPATH dst
  scanning source...
  reading p4 views
  collecting p4 changelists
  4 change a b/c
  sorting...
  converting...
  0 change a b/c
  $ hg -R dst log --template 'desc="{desc}" tags="{tags}" files="{files}"\n'
  desc="change a b/c" tags="" files="a b/c"
  desc="change b/c" tags="" files="b/c"
  desc="change a" tags="" files="a"
  desc="initial" tags="" files="a b/c"

interesting names
  $ echo dddd > "d d"
  $ mkdir " e"
  $ echo fff >" e/ f"
  $ p4 add "d d" " e/ f"
  //depot/test-mercurial-import/d d#1 - opened for add
  //depot/test-mercurial-import/ e/ f#1 - opened for add
  $ p4 submit -d "add d e f"
  Submitting change 5.
  Locking 2 files ...
  add //depot/test-mercurial-import/ e/ f#1
  add //depot/test-mercurial-import/d d#1
  Change 5 submitted.

convert again
  $ hg convert -s p4 $DEPOTPATH dst
  scanning source...
  reading p4 views
  collecting p4 changelists
  5 add d e f
  sorting...
  converting...
  0 add d e f
  $ hg -R dst log --template 'desc="{desc}" tags="{tags}" files="{files}"\n'
  desc="add d e f" tags="" files=" e/ f d d"
  desc="change a b/c" tags="" files="a b/c"
  desc="change b/c" tags="" files="b/c"
  desc="change a" tags="" files="a"
  desc="initial" tags="" files="a b/c"

empty commit message
  $ p4 edit a
  //depot/test-mercurial-import/a#3 - opened for edit
  $ echo aaaaa >> a
  $ p4 submit -d ""
  Submitting change 6.
  Locking 1 files ...
  edit //depot/test-mercurial-import/a#4
  Change 6 submitted.
  $ hg convert -s p4 $DEPOTPATH dst
  scanning source...
  reading p4 views
  collecting p4 changelists
  6 **empty changelist description**
  sorting...
  converting...
  0 

exit trap:
  stopping the p4 server
