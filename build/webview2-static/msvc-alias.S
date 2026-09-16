/* 把 MSVC 修饰名指到 msvc-shim.c 的普通 C 实现上。
   GAS 支持带引号的符号名,链接器 --defsym 不支持含 ? @ 的表达式,所以用汇编别名。 */
        .text
        .globl  "??2@YAPEAX_K@Z"
"??2@YAPEAX_K@Z":       jmp ms_new
        .globl  "??2@YAPEAX_KAEBUnothrow_t@std@@@Z"
"??2@YAPEAX_KAEBUnothrow_t@std@@@Z": jmp ms_new_nothrow
        .globl  "??3@YAXPEAX@Z"
"??3@YAXPEAX@Z":        jmp ms_del
        .globl  "??3@YAXPEAX_K@Z"
"??3@YAXPEAX_K@Z":      jmp ms_del_sz
        .globl  "??_U@YAPEAX_K@Z"
"??_U@YAPEAX_K@Z":      jmp ms_new_arr
        .globl  "??_V@YAXPEAX@Z"
"??_V@YAXPEAX@Z":       jmp ms_del_arr

/* std::nothrow 对象:空结构,占 1 字节,放只读段 */
        .section .rdata,"dr"
        .globl  "?nothrow@std@@3Unothrow_t@1@B"
"?nothrow@std@@3Unothrow_t@1@B":
        .byte   0
