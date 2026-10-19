/* VGA mode 13h through the BIOS, a palette entry through the DAC ports, and a pattern written
 * through a far pointer to A000:0000. The program leaves the picture on screen. */
#include <conio.h>
#include <dos.h>
#include <i86.h>

int main(void)
{
    union REGS r;
    unsigned char __far *screen = (unsigned char __far *)MK_FP(0xA000, 0);
    unsigned x, y;

    r.w.ax = 0x0013;
    int86(0x10, &r, &r);
    outp(0x3C8, 1);
    outp(0x3C9, 63);
    outp(0x3C9, 0);
    outp(0x3C9, 0);
    for (y = 0; y < 200; y++) {
        for (x = 0; x < 320; x++) {
            screen[y * 320 + x] = (unsigned char)(x ^ y);
        }
    }
    return 0;
}
