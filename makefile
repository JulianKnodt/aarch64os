PHONY:

empty:
	-@rm test.img
	dd bs=1048576 seek=1 of=test.img count=0

kill:
	killall qemu-system-aarch64
