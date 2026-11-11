; The boot code of the FAT32 volume on partition 1 of a Windows drive.
;
; The MBR loads sector 0 of the partition at 0000:7C00 and jumps to it, with
; the number of the drive in DL. This code finds the file BOOTMGR in the root
; directory, loads the whole file at 2000:0000, and jumps to it with the
; number of the drive in DL and this boot sector still at 0000:7C00. That is
; how Windows Setup starts from an ISO on a BIOS: BOOTMGR reads the BPB at
; 0000:7C00 to find its partition, and BOOTMGR then reads boot\BCD.
;
; The code is two parts of 512 bytes:
;
; - Part 1 is the code of sector 0 of the volume. Burnout keeps the BPB that
;   the format wrote, and puts the jump and the code of this part around it.
;   Part 1 reads part 2 from sector 2 of the volume, which the format leaves
;   empty, and jumps to it.
; - Part 2 goes at the start of sector 2 of the volume. It walks the root
;   directory and the table, and loads BOOTMGR.
;
; It reads with the extended functions of INT 13h only, by LBA, and it takes
; every number from the BPB: the sector size, the size of a cluster, the
; reserved sectors, the size of the table and the first sector of the
; partition. So it starts the volume at either sector size.
;
; Build: nasm -f bin -o fat32.bin fat32.asm
; scripts/boot-code.sh builds it and compares it with fat32.bin.

bits 16
cpu 386
org 0x7C00

; The fields of the BPB that this code reads, where the BIOS loads them.
%define BPB_BYTES_PER_SECTOR    0x7C00 + 0x0B   ; word
%define BPB_SECTORS_PER_CLUSTER 0x7C00 + 0x0D   ; byte
%define BPB_RESERVED_SECTORS    0x7C00 + 0x0E   ; word
%define BPB_TABLES              0x7C00 + 0x10   ; byte
%define BPB_HIDDEN_SECTORS      0x7C00 + 0x1C   ; dword
%define BPB_TABLE_SECTORS       0x7C00 + 0x24   ; dword
%define BPB_ROOT_CLUSTER        0x7C00 + 0x2C   ; dword
%define BPB_DRIVE               0x7C00 + 0x40   ; byte

; Memory that the code uses. Part 2 loads at 7E00, and a sector of 4096
; bytes fills it up to 8E00.
%define PART_2          0x7E00
%define TABLE_BUFFER    0x9000          ; one sector of the table
%define DIR_BUFFER      0xA000          ; one sector of the root directory
%define LOAD_SEGMENT    0x2000          ; BOOTMGR goes to 2000:0000

; Variables, below the stack.
%define VAR_TABLE_START 0x0500          ; dword: the first sector of the table
%define VAR_DATA_START  0x0504          ; dword: the sector of cluster 2
%define VAR_CACHED      0x0508          ; dword: the sector in TABLE_BUFFER
%define VAR_CLUSTER     0x050C          ; dword: the cluster in work
%define VAR_SEGMENT     0x0510          ; word: where the next sector of BOOTMGR goes
%define VAR_ENTRY       0x0512          ; word: the directory entry of BOOTMGR

%define END_OF_CHAIN    0x0FFFFFF8
%define BAD_CLUSTER     0x0FFFFFF7

; ---------------------------------------------------------------- part 1

        jmp     short part_1
        nop
        ; The BPB. Burnout keeps the bytes that the format wrote here.
        times 0x5A - ($ - $$) db 0

part_1:
        cli
        xor     ax, ax
        mov     ds, ax
        mov     es, ax
        mov     ss, ax
        mov     sp, 0x7C00
        sti
        cld
        ; BOOTMGR reads the number of the drive from the BPB.
        mov     [BPB_DRIVE], dl

        mov     ah, 0x41
        mov     bx, 0x55AA
        int     0x13
        jc      .no_lba
        cmp     bx, 0xAA55
        jne     .no_lba
        test    cl, 1
        jz      .no_lba

        mov     eax, 2
        mov     bx, PART_2
        call    read_sector
        cmp     word [PART_2 + 510], 0xAA55
        jne     .no_part_2
        jmp     part_2

.no_lba:
        mov     si, no_lba
        jmp     fail
.no_part_2:
        mov     si, no_part_2
        jmp     fail

; Read sector EAX of the volume to ES:BX.
read_sector:
        pushad
        add     eax, [BPB_HIDDEN_SECTORS]
        mov     [packet.lba], eax
        mov     [packet.offset], bx
        mov     [packet.segment], es
        mov     si, packet
        mov     ah, 0x42
        mov     dl, [BPB_DRIVE]
        int     0x13
        jc      .error
        popad
        ret
.error:
        mov     si, read_error
        ; fall through

; Print the text at DS:SI, which ends with a zero byte, and stop.
fail:
        xor     ax, ax
        mov     ds, ax
.next:
        lodsb
        test    al, al
        jz      .stop
        mov     ah, 0x0E
        mov     bx, 0x0007
        int     0x10
        jmp     .next
.stop:
        cli
        hlt
        jmp     .stop

; The packet of INT 13h function 42h: one sector.
packet:
        db      0x10, 0
        dw      1
.offset:  dw    0
.segment: dw    0
.lba:     dd    0, 0

no_lba:     db  "Burnout: the BIOS cannot read by LBA", 0
no_part_2:  db  "Burnout: the boot code is not whole", 0
read_error: db  "Burnout: the drive cannot be read", 0
; Two texts of part 2, here because part 1 has the room.
too_large:  db  "Burnout: BOOTMGR is too large for the memory", 0
bad_table:  db  "Burnout: the table of the volume is damaged", 0

        times 510 - ($ - $$) db 0
        dw      0xAA55

; ---------------------------------------------------------------- part 2

part_2:
        ; The table starts after the reserved sectors, and cluster 2 after
        ; every copy of the table.
        movzx   eax, word [BPB_RESERVED_SECTORS]
        mov     [VAR_TABLE_START], eax
        movzx   ecx, byte [BPB_TABLES]
        mov     eax, [BPB_TABLE_SECTORS]
        mul     ecx
        add     eax, [VAR_TABLE_START]
        mov     [VAR_DATA_START], eax
        mov     dword [VAR_CACHED], 0xFFFFFFFF

        ; Walk the root directory, one sector at a time.
        mov     eax, [BPB_ROOT_CLUSTER]
.dir_cluster:
        mov     [VAR_CLUSTER], eax
        call    cluster_sector
        movzx   ecx, byte [BPB_SECTORS_PER_CLUSTER]
.dir_sector:
        mov     bx, DIR_BUFFER
        call    read_sector
        call    find_bootmgr
        jnc     found
        inc     eax
        dec     ecx
        jnz     .dir_sector
        mov     eax, [VAR_CLUSTER]
        call    next_cluster
        jnc     .dir_cluster
        mov     si, no_bootmgr
        jmp     fail

; Look for BOOTMGR in the sector at DIR_BUFFER. Clear the carry flag and
; keep the entry in VAR_ENTRY when it is there.
find_bootmgr:
        pushad
        mov     di, DIR_BUFFER
        mov     cx, [BPB_BYTES_PER_SECTOR]
        shr     cx, 5
.entry:
        mov     al, [di]
        test    al, al
        jz      .end_of_directory
        cmp     al, 0xE5
        je      .skip
        mov     al, [di + 11]
        cmp     al, 0x0F                ; a part of a long name
        je      .skip
        test    al, 0x18                ; a directory or the label
        jnz     .skip
        push    cx
        push    di
        mov     si, bootmgr_name
        mov     cx, 11
        repe    cmpsb
        pop     di
        pop     cx
        je      .found
.skip:
        add     di, 32
        loop    .entry
        popad
        stc
        ret
.found:
        mov     [VAR_ENTRY], di
        popad
        clc
        ret
.end_of_directory:
        mov     si, no_bootmgr
        jmp     fail

found:
        mov     di, [VAR_ENTRY]

        ; The whole file goes to 2000:0000. It must end below the top of
        ; the memory that the BIOS gives, with room for one more cluster,
        ; because the load reads whole clusters.
        int     0x12                    ; AX: the memory below 640 KiB, in KiB
        movzx   ebx, ax
        shl     ebx, 10
        sub     ebx, LOAD_SEGMENT * 16
        movzx   eax, byte [BPB_SECTORS_PER_CLUSTER]
        movzx   ecx, word [BPB_BYTES_PER_SECTOR]
        mul     ecx
        sub     ebx, eax
        mov     eax, [di + 28]
        cmp     eax, ebx
        ja      .too_large

        mov     ax, [di + 20]
        shl     eax, 16
        mov     ax, [di + 26]
        mov     word [VAR_SEGMENT], LOAD_SEGMENT
.load_cluster:
        mov     [VAR_CLUSTER], eax
        call    cluster_sector
        movzx   ecx, byte [BPB_SECTORS_PER_CLUSTER]
.load_sector:
        push    es
        mov     es, [VAR_SEGMENT]
        xor     bx, bx
        call    read_sector
        pop     es
        mov     dx, [BPB_BYTES_PER_SECTOR]
        shr     dx, 4
        add     [VAR_SEGMENT], dx
        inc     eax
        dec     ecx
        jnz     .load_sector
        mov     eax, [VAR_CLUSTER]
        call    next_cluster
        jnc     .load_cluster

        ; Start BOOTMGR, the way the boot sector of Windows starts it.
        mov     dl, [BPB_DRIVE]
        mov     bp, 0x7C00
        jmp     LOAD_SEGMENT:0

.too_large:
        mov     si, too_large
        jmp     fail

; EAX: the first sector, in the volume, of cluster EAX.
cluster_sector:
        push    ecx
        push    edx
        sub     eax, 2
        movzx   ecx, byte [BPB_SECTORS_PER_CLUSTER]
        mul     ecx
        add     eax, [VAR_DATA_START]
        pop     edx
        pop     ecx
        ret

; EAX: the cluster after cluster EAX, from the table. The carry flag is set
; at the end of the chain.
next_cluster:
        push    ebx
        push    ecx
        push    edx
        shl     eax, 2                  ; the byte of the entry in the table
        movzx   ecx, word [BPB_BYTES_PER_SECTOR]
        xor     edx, edx
        div     ecx                     ; EAX: sector of the table, EDX: byte in it
        add     eax, [VAR_TABLE_START]
        cmp     eax, [VAR_CACHED]
        je      .cached
        mov     [VAR_CACHED], eax
        mov     bx, TABLE_BUFFER
        call    read_sector
.cached:
        mov     eax, [TABLE_BUFFER + edx]
        and     eax, 0x0FFFFFFF
        cmp     eax, END_OF_CHAIN
        jae     .end
        cmp     eax, 2
        jb      .bad
        cmp     eax, BAD_CLUSTER
        je      .bad
        clc
        jmp     .done
.end:
        stc
.done:
        pop     edx
        pop     ecx
        pop     ebx
        ret
.bad:
        mov     si, bad_table
        jmp     fail

bootmgr_name: db "BOOTMGR    "
no_bootmgr:   db "Burnout: the volume holds no BOOTMGR", 0

        times 1024 - 2 - ($ - $$) db 0
        dw      0xAA55
