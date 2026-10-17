/* Floating point on the 8087 (built with -fpi87): arithmetic, sqrt, sin, exp, printf %f. */
#include <stdio.h>
#include <math.h>

double average(const double *values, int count)
{
    double total = 0.0;
    int i;
    for (i = 0; i < count; i++) {
        total += values[i];
    }
    return total / count;
}

int main(void)
{
    static const double values[] = { 1.5, 2.25, 3.125, 4.0 };
    printf("%.6f %.6f %.6f %.4f\n", sqrt(2.0), sin(1.0), exp(1.0), average(values, 4));
    return 0;
}
