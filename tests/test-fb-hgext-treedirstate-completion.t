
  $ cat >> $HGRCPATH <<EOF
  > [extensions]
  > treedirstate=
  > [treedirstate]
  > useinnewrepos=True
  > EOF
  $ hg init repo
  $ cd repo
  $ echo file1 > file1
  $ echo file2 > file2
  $ mkdir -p dira dirb
  $ echo file3 > dira/file3
  $ echo file4 > dirb/file4
  $ echo file5 > dirb/file5
  $ hg ci -q -Am base

Test debugpathcomplete with just normal files

  $ hg debugpathcomplete f
  file1
  file2
  $ hg debugpathcomplete -f d
  dira/file3
  dirb/file4
  dirb/file5

Test debugpathcomplete with removed files

  $ hg rm dirb/file5
  $ hg debugpathcomplete -r d
  dirb
  $ hg debugpathcomplete -fr d
  dirb/file5
  $ hg rm dirb/file4
  $ hg debugpathcomplete -n d
  dira

