/*
 * list.c: a game's state behind pointers, for the mapping (D122). A linked list of actors on the far heap,
 * found through a far pointer in the data segment, each actor's pointer to the next in it; an array of 8 on
 * the far heap through another far pointer, and a far pointer into that array. Each pass moves the actors,
 * writes the array and points into it; at pass 50 the third actor leaves the list. A tick ends where `pass`
 * returns to main's loop.
 */
#include <malloc.h>
#include <stdio.h>

struct actor {
    short x;
    short hits;
    struct actor __far *next;
};

struct actor __far *actors;
short __far *table;
short __far *current;
unsigned short pass_count;

static void pass(void)
{
    struct actor __far *a;
    short i = 0;

    pass_count++;
    for (a = actors; a; a = a->next) {
        a->x += i + 1;
        i++;
    }
    table[pass_count & 7] = pass_count;
    current = &table[(pass_count * 3) & 7];
    if (pass_count == 50) {
        actors->next->next = actors->next->next->next;
    }
}

int main(void)
{
    struct actor __far *last = 0;
    short i;

    table = _fmalloc(8 * sizeof(short));
    for (i = 0; i < 8; i++) {
        table[i] = 0;
    }
    for (i = 0; i < 4; i++) {
        struct actor __far *a = _fmalloc(sizeof(struct actor));
        a->x = i * 10;
        a->hits = i;
        a->next = 0;
        if (last) {
            last->next = a;
        } else {
            actors = a;
        }
        last = a;
    }
    current = table;
    for (i = 0; i < 100; i++) {
        pass();
    }
    printf("%u\n", pass_count);
    return 0;
}
