/* Open Watcom's 32-bit C runtime, linked in: a table of the functions a DOS game of the 1990s may use
   (taking each one's address links it without calling it), for a signature pack of the runtime
   (rrc sigs build, sigs/README.md). It calls a few of them for its output. Built for CauseWay. */
#include <conio.h>
#include <ctype.h>
#include <direct.h>
#include <dos.h>
#include <fcntl.h>
#include <i86.h>
#include <io.h>
#include <malloc.h>
#include <math.h>
#include <setjmp.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static void *functions[] = {
    /* stdio */
    (void *)printf, (void *)sprintf, (void *)vsprintf, (void *)fprintf, (void *)vfprintf, (void *)sscanf,
    (void *)fscanf, (void *)fopen, (void *)fclose, (void *)fread, (void *)fwrite, (void *)fseek, (void *)ftell,
    (void *)fgets, (void *)fputs, (void *)fgetc, (void *)fputc, (void *)(ungetc), (void *)fflush,
    (void *)rewind, (void *)(feof), (void *)(ferror), (void *)setvbuf, (void *)puts, (void *)(putchar),
    (void *)(getchar), (void *)remove, (void *)rename, (void *)tmpnam, (void *)freopen, (void *)fdopen,
    (void *)fileno,
    /* string */
    (void *)strcpy, (void *)strncpy, (void *)strcat, (void *)strncat, (void *)strcmp, (void *)strncmp,
    (void *)stricmp, (void *)strnicmp, (void *)strlen, (void *)strchr, (void *)strrchr, (void *)strstr,
    (void *)strdup, (void *)strupr, (void *)strlwr, (void *)strrev, (void *)strtok, (void *)strspn,
    (void *)strcspn, (void *)strpbrk, (void *)strset, (void *)memcpy, (void *)memmove, (void *)memset,
    (void *)memcmp, (void *)memchr, (void *)memicmp, (void *)strerror,
    /* stdlib */
    (void *)malloc, (void *)calloc, (void *)realloc, (void *)free, (void *)atoi, (void *)atol, (void *)atof,
    (void *)strtol, (void *)strtoul, (void *)strtod, (void *)itoa, (void *)ltoa, (void *)utoa, (void *)ultoa,
    (void *)qsort, (void *)bsearch, (void *)rand, (void *)srand, (void *)(abs), (void *)(labs), (void *)div,
    (void *)ldiv, (void *)getenv, (void *)putenv, (void *)exit, (void *)_exit, (void *)atexit, (void *)abort,
    (void *)system, (void *)_splitpath, (void *)_makepath, (void *)_fullpath, (void *)_msize, (void *)_expand,
    (void *)_heapchk, (void *)_heapmin,
    /* math */
    (void *)sin, (void *)cos, (void *)tan, (void *)asin, (void *)acos, (void *)atan, (void *)atan2,
    (void *)sinh, (void *)cosh, (void *)tanh, (void *)sqrt, (void *)pow, (void *)exp, (void *)log,
    (void *)log10, (void *)floor, (void *)ceil, (void *)(fabs), (void *)fmod, (void *)modf, (void *)frexp,
    (void *)ldexp, (void *)hypot,
    /* ctype */
    (void *)(toupper), (void *)(tolower), (void *)(isalpha), (void *)(isdigit), (void *)(isspace),
    (void *)(isupper), (void *)(islower), (void *)(isalnum), (void *)(isxdigit), (void *)(isprint),
    (void *)(ispunct), (void *)(iscntrl),
    /* time */
    (void *)time, (void *)clock, (void *)localtime, (void *)gmtime, (void *)mktime, (void *)difftime,
    (void *)strftime, (void *)ctime, (void *)asctime,
    /* io, fcntl, direct */
    (void *)open, (void *)close, (void *)read, (void *)write, (void *)lseek, (void *)tell,
    (void *)filelength, (void *)access, (void *)eof, (void *)creat, (void *)unlink, (void *)setmode,
    (void *)chsize, (void *)sopen, (void *)getcwd, (void *)chdir, (void *)mkdir, (void *)rmdir,
    /* dos, i86, conio */
    (void *)_dos_findfirst, (void *)_dos_findnext, (void *)_dos_getdate, (void *)_dos_gettime,
    (void *)_dos_getdiskfree, (void *)_dos_getdrive, (void *)_dos_setdrive, (void *)_dos_open,
    (void *)_dos_close, (void *)_dos_read, (void *)_dos_write, (void *)_dos_getvect, (void *)_dos_setvect,
    (void *)int386, (void *)int386x, (void *)intdos, (void *)segread, (void *)delay, (void *)sound,
    (void *)nosound, (void *)(inp), (void *)(outp), (void *)(inpw), (void *)(outpw), (void *)kbhit,
    (void *)getch, (void *)getche, (void *)cputs, (void *)cprintf,
    /* setjmp, signal */
    (void *)longjmp, (void *)signal, (void *)raise,
};

static int compare(const void *a, const void *b)
{
    return *(const int *)a - *(const int *)b;
}

int main(void)
{
    int values[5] = { 5, 3, 9, 1, 7 };
    char text[64];
    qsort(values, 5, sizeof values[0], compare);
    sprintf(text, "%d %d %d %d %d", values[0], values[1], values[2], values[3], values[4]);
    printf("clib32: %u functions; %s; %.3f\n", sizeof functions / sizeof functions[0], text, sqrt(2.0));
    return 0;
}
