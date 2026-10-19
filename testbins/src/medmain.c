/* Medium model, two source files: far calls between code segments, a switch compiled to a
 * jump table, and recursion (see medlib.c). */
#include <stdio.h>

int classify(int n);
long fibonacci(int n);

int main(void)
{
    int i;
    long total = 0;
    for (i = 0; i < 10; i++) {
        total += classify(i);
    }
    printf("%ld %ld\n", total, fibonacci(20));
    return 0;
}
