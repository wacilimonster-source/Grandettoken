/* MinGW 链接 MSVC 版 WebView2LoaderStatic.lib 所需的最小垫片。
   MSVC 编译的加载器依赖 MSVC CRT 里几样 gcc/MinGW 没有的东西:
   /GS 栈保护 cookie、C++ 线程安全静态初始化原语、operator new/delete 与 std::nothrow。
   这些符号必须用 MSVC 的修饰名(GCC 用 Itanium 修饰),
   函数名在 msvc-alias.S 里用汇编别名指到本文件的实现上。 */
#include <windows.h>
#include <stddef.h>
#include <stdlib.h>

/* ── /GS 栈保护 ── */
uintptr_t __security_cookie = 0x2B992DDFA232ULL;
void __security_check_cookie(uintptr_t cookie) {
  /* 只做比较,不匹配就结束进程(与 MSVC 的 __report_gsfailure 等效) */
  if (cookie != __security_cookie) abort();
}

/* ── C++ 线程安全魔法静态(MSVC 原语) ──
   cookie[0] = 状态(0 未初始化 / -1 初始化中 / 1 已完成), cookie[1] = 完成时的 epoch。
   生成代码的约定:caller 先调 header,若返回时状态为 -1 说明自己是初始化者(持锁),
   初始化完调 footer 交还锁。 */
int _Init_thread_epoch = 0;

static volatile LONG g_lock = 0;
static void lock(void) { while (InterlockedCompareExchange(&g_lock, 1, 0) != 0) Sleep(0); }
static void unlock(void) { InterlockedExchange(&g_lock, 0); }

void _Init_thread_header(int *once);
void _Init_thread_footer(int *once);

void _Init_thread_header(int *once) {
  lock();
  while (once[0] == -1) { /* 别的线程正在初始化:让出 CPU 等它完成 */
    unlock();
    Sleep(1);
    lock();
  }
  if (once[0] == 0) { once[0] = -1; return; } /* 我是初始化者,持锁返回 */
  unlock();
}

void _Init_thread_footer(int *once) {
  once[1] = _Init_thread_epoch;
  once[0] = 1;
  unlock();
}

/* ── C++ operator new / delete 与 std::nothrow ──
   MSVC 的修饰名在链接期用 --defsym 指到这些函数上(见 build 脚本)。 */
const char ms_nothrow[1] = { 0 };

void *ms_new(size_t sz) { return malloc(sz ? sz : 1); }
void *ms_new_nothrow(size_t sz, const void *nt) { (void)nt; return malloc(sz ? sz : 1); }
void ms_del(void *p) { free(p); }
void ms_del_sz(void *p, size_t sz) { (void)sz; free(p); }
void *ms_new_arr(size_t sz) { return malloc(sz ? sz : 1); }
void ms_del_arr(void *p) { free(p); }
