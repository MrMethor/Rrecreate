/* An interrupt handler: INT 08h hooked with an __interrupt function, the PIT reprogrammed to
 * 1000 Hz, 500 ticks counted, then everything put back. */
#include <stdio.h>
#include <conio.h>
#include <dos.h>

static volatile unsigned long ticks;
static void (__interrupt __far *old_timer)(void);

static void __interrupt __far on_timer(void)
{
    ticks++;
    outp(0x20, 0x20);
}

static void set_divisor(unsigned divisor)
{
    outp(0x43, 0x36);
    outp(0x40, divisor & 0xFF);
    outp(0x40, divisor >> 8);
}

int main(void)
{
    unsigned long counted;

    old_timer = _dos_getvect(0x08);
    _dos_setvect(0x08, on_timer);
    set_divisor(1193);
    while (ticks < 500) {
    }
    _disable();
    set_divisor(0);
    _dos_setvect(0x08, old_timer);
    counted = ticks;
    _enable();
    printf("ticks %lu\n", counted);
    return 0;
}
