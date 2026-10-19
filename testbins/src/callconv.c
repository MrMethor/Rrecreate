/* The calling conventions a 16-bit DOS program meets: cdecl and pascal (arguments on the
 * stack, cleaned up by the caller or the callee) and Watcom's default register convention
 * (AX, DX, BX, CX), plus a long multiply done by a runtime helper. */
#include <stdio.h>

int __cdecl digits_cdecl(int a, int b, int c)
{
    return a * 100 + b * 10 + c;
}

int __pascal digits_pascal(int a, int b, int c)
{
    return a * 100 + b * 10 + c;
}

int digits_register(int a, int b, int c, int d)
{
    return a * 1000 + b * 100 + c * 10 + d;
}

long multiply(long a, long b)
{
    return a * b;
}

int sum(const int *values, int count)
{
    int total = 0;
    while (count-- > 0) {
        total += *values++;
    }
    return total;
}

int main(void)
{
    static const int values[] = { 3, 1, 4, 1, 5, 9, 2, 6 };
    printf("%d %d %d %ld %d\n", digits_cdecl(1, 2, 3), digits_pascal(4, 5, 6), digits_register(1, 2, 3, 4),
           multiply(100000L, 7L), sum(values, 8));
    return 0;
}
