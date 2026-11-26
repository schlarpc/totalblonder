# eedump: bounded, read-only dump of the EQAM-420B 8 KB EEPROM mirror.
# Does what every stock "BT" program does to read the settings (update_config 0x1000228c..0x100022b0):
#   wait for bit 0 of 0x12000810, then copy 0x2000 bytes from 0x13010000 with word loads.
# Then writes the copy to /tmp/ee.bin and exits (0 = ok, 1 = short write, 2 = open failed).
# No stores to any FPGA address, no mailbox access, no data/bss segment, no relocations.
# Syscalls as in the stock uClibc stubs: r2 = 0, r3 = number, args r4..r6, trap, result in r2.
        .text
        .global _start
_start:
        addi    sp, sp, -8208           # 8192-byte buffer on the stack (+16 spare)
        movhi   r3, 4608                # 0x12000810, same two instructions as stock
        addi    r3, r3, 2064
1:      ldwio   r2, 0(r3)
        andi    r2, r2, 1
        beq     r2, zero, 1b
        mov     r4, sp
        movhi   r5, 4865                # 0x13010000
        movi    r6, 2048                # words
2:      ldw     r7, 0(r5)
        stw     r7, 0(r4)
        addi    r5, r5, 4
        addi    r4, r4, 4
        addi    r6, r6, -1
        bne     r6, zero, 2b
        nextpc  r4
3:      addi    r4, r4, path - 3b       # position-independent address of the file name
        movi    r5, 0x241               # O_WRONLY | O_CREAT | O_TRUNC
        movi    r6, 0x1a4               # 0644
        movi    r2, 0
        movi    r3, 5                   # open
        trap
        blt     r2, zero, 5f
        mov     r16, r2
        mov     r4, r16
        mov     r5, sp
        movi    r6, 8192
        movi    r2, 0
        movi    r3, 4                   # write
        trap
        mov     r17, r2
        mov     r4, r16
        movi    r2, 0
        movi    r3, 6                   # close
        trap
        cmpnei  r4, r17, 8192           # exit status 0 if all 8192 bytes were written
4:      movi    r2, 0
        movi    r3, 1                   # exit
        trap
        br      4b
5:      movi    r4, 2
        br      4b
path:   .asciz  "/tmp/ee.bin"
        .balign 4
