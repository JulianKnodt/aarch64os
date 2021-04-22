PHONY:

empty:
	dd bs=1048576 seek=1 of=test.img count=0
