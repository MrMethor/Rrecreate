/* Large model: far code and data pointers, the far heap, a 40 000-byte block, and qsort
 * calling back through a function pointer. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <malloc.h>

struct item {
    char name[12];
    int score;
};

static int by_score(const void *a, const void *b)
{
    const struct item *x = a;
    const struct item *y = b;
    return x->score - y->score;
}

int main(void)
{
    static const char *names[] = { "delta", "alpha", "echo", "bravo", "charlie" };
    static const int scores[] = { 40, 10, 50, 20, 30 };
    struct item *items = malloc(5 * sizeof *items);
    char __far *big = _fmalloc(40000U);
    unsigned i;
    unsigned long total = 0;

    if (items == NULL || big == NULL) {
        printf("out of memory\n");
        return 1;
    }
    for (i = 0; i < 5; i++) {
        strcpy(items[i].name, names[i]);
        items[i].score = scores[i];
    }
    qsort(items, 5, sizeof *items, by_score);
    for (i = 0; i < 40000U; i++) {
        big[i] = (char)(i * 7);
    }
    for (i = 0; i < 40000U; i += 1000) {
        total += (unsigned char)big[i];
    }
    for (i = 0; i < 5; i++) {
        printf("%s ", items[i].name);
    }
    printf("%lu\n", total);
    _ffree(big);
    free(items);
    return 0;
}
