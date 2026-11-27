.section .text
.global _start

_start:
    # SYS_WRITE(1, msg1, len1)
    movq $1, %rax
    movq $1, %rdi
    leaq msg1(%rip), %rsi
    movq $(msg1_end - msg1), %rdx
    syscall

    # SYS_GETPID()
    movq $20, %rax
    syscall

    # SYS_UPTIME()
    movq $96, %rax
    syscall

    # SYS_WRITE(1, msg2, len2)
    movq $1, %rax
    movq $1, %rdi
    leaq msg2(%rip), %rsi
    movq $(msg2_end - msg2), %rdx
    syscall

    # SYS_EXIT(0)
    movq $60, %rax
    xorq %rdi, %rdi
    syscall

1:
    jmp 1b

.section .rodata
msg1:
    .ascii "Hello from WMOS Ring-3 ELF64 userland!\n"
msg1_end:

msg2:
    .ascii "Syscall ABI verified (CPL=3 -> Ring 0 -> CPL=3).\n"
msg2_end:
