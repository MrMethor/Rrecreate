/* flat32.c's program under DOS/4GW (an LE executable whose stub runs DOS4GW.EXE, which loads it).
   It waits a quarter of a second before and after its work, so a tool that looks for its objects
   while it runs finds them. */
#include <stdio.h>
#include <time.h>

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

static void wait(void)
{
    clock_t start = clock();
    while (clock() - start < CLOCKS_PER_SEC / 4) {
    }
}

int main(void)
{
    int total;
    wait();
    total = sum(square, 16);
    wait();
    printf("flat4gw: %d from %d calls\n", total, calls);
    return total == 1240 ? 32 : 1;
}
