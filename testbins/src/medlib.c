/* The second code segment of the medium-model program (see medmain.c). */

int classify(int n)
{
    switch (n) {
    case 0: return 10;
    case 1: return 11;
    case 2: return 13;
    case 3: return 17;
    case 4: return 19;
    case 5: return 23;
    case 6: return 29;
    case 7: return 31;
    default: return -1;
    }
}

long fibonacci(int n)
{
    return n < 2 ? n : fibonacci(n - 1) + fibonacci(n - 2);
}
