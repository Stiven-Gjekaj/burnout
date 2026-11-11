; A file that stands in for BOOTMGR in the test of the boot code.
;
; scripts/bios-boot-test.sh puts it on partition 1 of a drive image as
; `bootmgr`, and starts the image in QEMU with SeaBIOS. The boot code of the
; MBR and of the volume must load the whole file at 2000:0000 and start it,
; as they start BOOTMGR. The file then writes one line to the first serial
; port and stops QEMU through its isa-debug-exit device:
;
;   PROBE cs=2000 dl=80 bpb-dl=80 hidden=00000800 signature=ok end=ok
;
; `end=ok` says that the last 16 bytes of the file are in memory where they
; belong, so the whole file was loaded. The file is about as large as the
; BOOTMGR of Windows 10, so it fills many clusters.
;
; Build: nasm -f bin -o probe.bin probe.asm

bits 16
cpu 386
org 0

%define SIZE        409600              ; bytes of the whole file
%define LOAD        0x20000             ; where the boot code loads it
%define END_SEGMENT ((LOAD + SIZE - 16) >> 4)
%define END_OFFSET  ((LOAD + SIZE - 16) & 0x0F)
%define SERIAL      0x3F8
%define EXIT_PORT   0xF4

start:
        mov     [cs:drive], dl
        mov     ax, cs
        mov     ds, ax

        mov     si, text_cs
        call    print
        mov     ax, cs
        call    print_word

        mov     si, text_dl
        call    print
        mov     al, [drive]
        call    print_byte

        xor     ax, ax
        mov     es, ax
        mov     si, text_bpb_dl
        call    print
        mov     al, [es:0x7C40]
        call    print_byte

        mov     si, text_hidden
        call    print
        mov     ax, [es:0x7C1E]
        call    print_word
        mov     ax, [es:0x7C1C]
        call    print_word

        mov     si, text_signature
        call    print
        cmp     word [es:0x7DFE], 0xAA55
        call    print_result

        mov     si, text_end
        call    print
        mov     ax, END_SEGMENT
        mov     es, ax
        mov     di, END_OFFSET
        mov     si, expected_end
        mov     cx, 16
        repe    cmpsb
        call    print_result

        mov     si, text_newline
        call    print
        mov     al, 0x10
        out     EXIT_PORT, al
.stop:
        cli
        hlt
        jmp     .stop

; Print "ok" when the zero flag is set, and "bad" when it is not.
print_result:
        mov     si, text_ok
        je      print
        mov     si, text_bad
        ; fall through

; Print the text at DS:SI, which ends with a zero byte.
print:
        push    dx
        mov     dx, SERIAL
.next:
        lodsb
        test    al, al
        jz      .done
        out     dx, al
        jmp     .next
.done:
        pop     dx
        ret

print_word:
        push    ax
        mov     al, ah
        call    print_byte
        pop     ax
        ; fall through

print_byte:
        push    ax
        shr     al, 4
        call    print_digit
        pop     ax
        ; fall through

print_digit:
        push    ax
        push    dx
        and     al, 0x0F
        add     al, '0'
        cmp     al, '9'
        jbe     .out
        add     al, 'a' - '9' - 1
.out:
        mov     dx, SERIAL
        out     dx, al
        pop     dx
        pop     ax
        ret

drive:          db 0
; What the last 16 bytes of the file hold. The check compares the memory at
; the end of the file with this copy at its start.
expected_end:   db "BURNOUT-PROBE-OK"
text_cs:        db "PROBE cs=", 0
text_dl:        db " dl=", 0
text_bpb_dl:    db " bpb-dl=", 0
text_hidden:    db " hidden=", 0
text_signature: db " signature=", 0
text_end:       db " end=", 0
text_ok:        db "ok", 0
text_bad:       db "bad", 0
text_newline:   db 13, 10, 0

        ; A pattern that changes with the place, so a cluster loaded twice
        ; or out of order does not pass as the right one.
%assign i ($ - $$)
%rep SIZE - 16 - ($ - $$)
        db      (i * 7 + (i >> 9)) & 0xFF
%assign i i + 1
%endrep

end_marker:
        db      "BURNOUT-PROBE-OK"
