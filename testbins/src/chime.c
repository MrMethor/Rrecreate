/* A test program for M6, sound: a chime played on the PC speaker and on the OPL (an AdLib's FM chip), whose
 * port's sound events are compared with the original's.
 *
 * One loop waits for the timer (70 Hz), then runs a pass. Space plays the chosen note for NOTE_TICKS passes:
 * the speaker sounds it (the timer's channel 2 at the note's divisor, port 61h's gate and data bits on) and
 * the OPL's channel 0 plays it (its frequency, the key on); then both stop. A note played while one sounds
 * cuts it off. Up and Down choose the note, of eight. Esc ends it; the exit code is the notes played.
 *
 * The OPL's channel 0 is set up as an instrument once, before the loop. The state is in the named globals
 * below, so the linker map gives every item's address. */
#include <conio.h>
#include <dos.h>
#include <i86.h>

#define NOTES 8
#define NOTE_TICKS 20
#define BLOCK 5

/* C5 to C6: the speaker's divisors (1193182 / Hz) and the OPL's frequency numbers in block 5. */
static const unsigned short divisors[NOTES] = { 2281, 2032, 1810, 1709, 1522, 1356, 1208, 1140 };
static const unsigned short fnums[NOTES] = { 0x159, 0x183, 0x1B2, 0x1CC, 0x205, 0x244, 0x28B, 0x2B2 };

volatile unsigned short timer_count;
volatile unsigned char key_down[128];

unsigned short last_count;
unsigned short pass_count;
unsigned char note;      /* the note Space plays, 0 to NOTES - 1 */
unsigned char playing;   /* passes left of the note sounding */
unsigned char sounding;  /* the note sounding */
unsigned char keys_held; /* bit per key (Space, Up, Down) down at the last pass */
unsigned short played;   /* notes played */

static void (__interrupt __far *old_timer)(void);
static void (__interrupt __far *old_keyboard)(void);

static void __interrupt __far on_timer(void)
{
    timer_count++;
    outp(0x20, 0x20);
}

static void __interrupt __far on_keyboard(void)
{
    unsigned char scan = inp(0x60);
    key_down[scan & 0x7F] = (scan & 0x80) ? 0 : 1;
    outp(0x20, 0x20);
}

/* An OPL register written, with the waits an AdLib needs after the index and the value. */
static void opl(unsigned char reg, unsigned char value)
{
    unsigned i;
    outp(0x388, reg);
    for (i = 0; i < 6; i++) {
        inp(0x388);
    }
    outp(0x389, value);
    for (i = 0; i < 35; i++) {
        inp(0x388);
    }
}

static void speaker_on(unsigned divisor)
{
    outp(0x43, 0xB6);
    outp(0x42, divisor & 0xFF);
    outp(0x42, divisor >> 8);
    outp(0x61, inp(0x61) | 3);
}

static void speaker_off(void)
{
    outp(0x61, inp(0x61) & ~3);
}

/* Channel 0's instrument: a plain tone, the modulator quiet, the carrier at full level. */
static void instrument(void)
{
    opl(0x20, 0x01);
    opl(0x40, 0x3F);
    opl(0x60, 0xF0);
    opl(0x80, 0x77);
    opl(0x23, 0x01);
    opl(0x43, 0x00);
    opl(0x63, 0xF0);
    opl(0x83, 0x77);
    opl(0xC0, 0x00);
}

void wait_tick(void)
{
    while (timer_count == last_count) {
    }
}

void pass(void)
{
    unsigned char held = 0;
    last_count = timer_count;
    pass_count++;
    if (key_down[0x39]) {
        held |= 1;
    }
    if (key_down[0x48]) {
        held |= 2;
    }
    if (key_down[0x50]) {
        held |= 4;
    }
    if ((held & 2) && !(keys_held & 2) && note < NOTES - 1) {
        note++;
    }
    if ((held & 4) && !(keys_held & 4) && note > 0) {
        note--;
    }
    if (playing) {
        playing--;
        if (!playing) {
            speaker_off();
            opl(0xB0, (BLOCK << 2) | (fnums[sounding] >> 8));
        }
    }
    if ((held & 1) && !(keys_held & 1)) {
        sounding = note;
        playing = NOTE_TICKS;
        played++;
        speaker_on(divisors[note]);
        opl(0xA0, fnums[note] & 0xFF);
        opl(0xB0, 0x20 | (BLOCK << 2) | (fnums[note] >> 8));
    }
    keys_held = held;
}

static void set_divisor(unsigned divisor)
{
    outp(0x43, 0x36);
    outp(0x40, divisor & 0xFF);
    outp(0x40, divisor >> 8);
}

int main(void)
{
    old_timer = _dos_getvect(0x08);
    old_keyboard = _dos_getvect(0x09);
    _disable();
    _dos_setvect(0x08, on_timer);
    _dos_setvect(0x09, on_keyboard);
    set_divisor(17045); /* 1193182 / 70 */
    _enable();

    instrument();
    while (!key_down[0x01]) {
        wait_tick();
        pass();
    }

    speaker_off();
    opl(0xB0, 0);
    _disable();
    set_divisor(0);
    _dos_setvect(0x08, old_timer);
    _dos_setvect(0x09, old_keyboard);
    _enable();
    return played & 0xFF;
}
