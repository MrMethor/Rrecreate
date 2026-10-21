/* A 32-bit flat-model program under CauseWay (an LE executable): code and data in objects the
   extender places, a global table, a function called through a pointer, and an exit code. */
#include <stdio.h>
#include <stdlib.h>

int squares[16];
static int calls;

static int square(int n)
{
    calls++;
    return n * n;
}

int sum(int (*f)(int), int count)
{
    int i, total = 0;
    for (i = 0; i < count; i++) {
        squares[i] = f(i);
        total += squares[i];
    }
    return total;
}

int main(void)
{
    int total = sum(square, 16);
    printf("flat32: %d from %d calls\n", total, calls);
    return total == 1240 ? 32 : 1;
}
