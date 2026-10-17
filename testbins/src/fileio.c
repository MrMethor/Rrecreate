/* File I/O through the C runtime: create, write, seek, read, rename, delete. */
#include <stdio.h>
#include <string.h>

int main(void)
{
    FILE *f;
    char text[32];
    long size;

    f = fopen("TEST.DAT", "wb");
    if (f == NULL) {
        printf("can't create\n");
        return 1;
    }
    fputs("hello, file", f);
    fclose(f);

    f = fopen("TEST.DAT", "rb");
    fseek(f, 0, SEEK_END);
    size = ftell(f);
    fseek(f, 7, SEEK_SET);
    memset(text, 0, sizeof text);
    fread(text, 1, 4, f);
    fclose(f);

    rename("TEST.DAT", "MOVED.DAT");
    printf("%ld %s %d\n", size, text, remove("MOVED.DAT"));
    return 0;
}
