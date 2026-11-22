#!/bin/bash
A=/root/octet-test/kt/all-rounds.log
echo "rounds whose kill left an in-flight .octet.* temporary: $(grep -c 'note: temporary file of the interrupted switch' $A)"
echo "rounds whose kill left a temporary (incl. multiple per round):"; grep -B0 'temporary file of' $A | sed 's/.*switch: //' | head -5
echo "--- script rounds: last script started before the kill, and what the completing run executed"
awk '/^=== round/{r=$0; s=($0 ~ /mode script/)} s && /executing/{print "  " substr(r,1,40) " :: " $0}' $A | head -40
echo "--- completing runs that executed a script (re-runs after the kill):"
grep -c '^    |   executing' $A
grep 'finishing an interrupted switch' $A | wc -l
grep -h 'switch finished' $A | sort | uniq -c | sort -rn | head -5
