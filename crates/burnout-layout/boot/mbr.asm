; The boot code of the MBR that Burnout writes.
;
; A BIOS loads sector 0 of the drive at 0000:7C00 and jumps to it, with the
; number of the drive in DL. This code moves itself to 0000:0600, finds the
; active partition in the table, reads the first sector of that partition to
; 0000:7C00, and jumps to it. DL keeps the number of the drive, and DS:SI
; and DS:BP point at the entry of the table, as every MBR gives them.
;
; It reads with the extended functions of INT 13h only, which every BIOS of
; this century has. The address is the LBA in the table, so a partition past
; 8 GB starts too.
;
; The code fills bytes 0 to 439 at most. The disk signature, the table and
; the boot signature come after it, and Burnout writes those.
;
; Build: nasm -f bin -o mbr.bin mbr.asm
; scripts/boot-code.sh builds it and compares it with mbr.bin.

bits 16
org 0x0600

%define LOADED      0x7C00          ; where the BIOS and this code load a sector
%define TABLE       0x0600 + 446    ; the table, after the move
%define ACTIVE      0x80

start:
        cli
        xor     ax, ax
        mov     ss, ax
        mov     sp, LOADED
        mov     ds, ax
        mov     es, ax
        sti
        cld
        ; Move the 512 bytes from 7C00 to 0600, so the boot sector of the
        ; partition can go to 7C00.
        mov     si, LOADED
        mov     di, 0x0600
        mov     cx, 256
        rep     movsw
        jmp     0:moved

moved:
        mov     [drive], dl

        ; The first entry that is marked active.
        mov     si, TABLE
        mov     cx, 4
.next:
        cmp     byte [si], ACTIVE
        je      .found
        add     si, 16
        loop    .next
        mov     si, no_active
        jmp     fail

.found:
        ; The extended read must be there.
        mov     ah, 0x41
        mov     bx, 0x55AA
        mov     dl, [drive]
        int     0x13
        jc      .no_lba
        cmp     bx, 0xAA55
        jne     .no_lba
        test    cl, 1
        jz      .no_lba

        ; Read the first sector of the partition to 7C00.
        mov     eax, [si + 8]
        mov     [packet.lba], eax
        push    si
        mov     si, packet
        mov     ah, 0x42
        mov     dl, [drive]
        int     0x13
        pop     si
        jc      .read_error

        cmp     word [LOADED + 510], 0xAA55
        jne     .no_code

        mov     bp, si
        mov     dl, [drive]
        jmp     0:LOADED

.no_lba:
        mov     si, no_lba
        jmp     fail
.read_error:
        mov     si, read_error
        jmp     fail
.no_code:
        mov     si, no_code
        ; fall through

; Print the text at DS:SI, which ends with a zero byte, and stop.
fail:
        lodsb
        test    al, al
        jz      .stop
        mov     ah, 0x0E
        mov     bx, 0x0007
        int     0x10
        jmp     fail
.stop:
        cli
        hlt
        jmp     .stop

; The packet of INT 13h function 42h: one sector to 0000:7C00.
packet:
        db      0x10, 0
        dw      1
        dw      LOADED, 0
.lba:   dd      0, 0

drive:  db      0

no_active:  db  "Burnout: no partition is active", 0
no_lba:     db  "Burnout: the BIOS cannot read by LBA", 0
read_error: db  "Burnout: the partition cannot be read", 0
no_code:    db  "Burnout: the partition holds no boot code", 0

        times 440 - ($ - $$) db 0
