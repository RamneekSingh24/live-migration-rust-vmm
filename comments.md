We are running the command below which counts till 46 every second:
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46; do echo $i; sleep 1 ;done

In the leftmost terminal the vm is started and the command is run

In the middle terminal the live migration is started.

After a few iterations, the vm will stop in the left terminal and will start running in the middle terminal...

The rightmost terminal is also running the same command, to show the lag in the vm counting due to migration...