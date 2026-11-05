#!/bin/sh
# A fake agy that lists no hooks and prints no version (see a_hook_that_was_not_loaded_fails_the_turn).
# A checked-in file, not one the test writes: a freshly written script can fail to exec with
# ETXTBSY while another test thread forks.
echo "$*" >> args.log
exit 0
