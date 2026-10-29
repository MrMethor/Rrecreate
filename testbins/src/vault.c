/* A test program for M5, the game's data: a game that loads what it shows and plays from its data file,
 * VAULT.DAT (testbins/data.py writes it), an archive of named members, each stored, run-length coded or
 * LZSS coded.
 *
 * It runs two loops, each waiting for the timer (70 Hz) and then running a pass. The title loop shows the
 * title picture (TITLE, run-length coded, decoded into a buffer on the heap) in mode 13h with its palette
 * (PALETTE, stored) until Enter, which loads the first level. The play loop moves the player a cell for each
 * arrow key pressed on the level's map (LEVEL1 and LEVEL2, LZSS coded, decoded onto the heap): a wall stops
 * it and is counted, the exit loads the next level in the pass, or ends the game after the last. Esc ends it
 * too; the exit code is the moves' low byte.
 *
 * The state is in the named globals below, so the linker map gives every item's address; the map and the
 * picture are on the far heap, through far pointers. */
#include <conio.h>
#include <dos.h>
#include <i86.h>
#include <malloc.h>
#include <string.h>

#define CELLS 32
#define FLOOR 0
#define WALL 1
#define EXIT 2
#define START 3
#define LEVELS 2
#define PICTURE (320U * 200U)

/* The LZSS of Okumura's lzss.c: a 4096-byte ring of spaces, matches of 3 to 18. */
#define RING 4096
#define LONGEST 18
#define SHORTEST 2

/* VAULT.DAT: "VALT", the count of members, then a directory entry each. */
struct entry {
    char name[8];
    unsigned long offset;
    unsigned long packed;
    unsigned long size;
    unsigned short method; /* 0 stored, 1 run-length, 2 LZSS */
    unsigned short reserved;
};

volatile unsigned short timer_count;
volatile unsigned char key_down[128];

unsigned short last_count;
unsigned short pass_count;
unsigned char scene;           /* 0 the title, 1 play, 2 over */
unsigned short title_passes;
unsigned char level;           /* from 1 */
unsigned char player_x;
unsigned char player_y;
unsigned short moves;
unsigned short bumps;
unsigned char keys_held;       /* bit per arrow key (left, right, up, down) down at the last pass */
unsigned char loads;           /* members loaded */
unsigned short entries;
struct entry directory[8];
unsigned char palette[768];
unsigned char __far *map;      /* the level's cells, CELLS x CELLS, on the heap */
unsigned char __far *picture;  /* the title, 320 x 200, on the heap */

static int archive;
static unsigned char ring[RING];
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

/* PCX's run-length code: a byte with its top two bits set repeats the next byte its low six times. */
void unpack_runs(const unsigned char __far *from, unsigned packed, unsigned char __far *to, unsigned size)
{
    unsigned i = 0, o = 0;
    while (i < packed && o < size) {
        unsigned char b = from[i++];
        if ((b & 0xC0) == 0xC0) {
            unsigned n = b & 0x3F;
            unsigned char v = from[i++];
            while (n-- && o < size) {
                to[o++] = v;
            }
        } else {
            to[o++] = b;
        }
    }
}

/* Okumura's LZSS: a flag byte for eight items, low bit first; 1 a literal, 0 a match: 12 bits of its place
 * in the ring and 4 of its length less 3. */
void unpack_lzss(const unsigned char __far *from, unsigned packed, unsigned char __far *to, unsigned size)
{
    unsigned i = 0, o = 0, r = RING - LONGEST, flags = 0, k;
    memset(ring, ' ', RING - LONGEST);
    for (;;) {
        flags >>= 1;
        if ((flags & 0x100) == 0) {
            if (i >= packed) {
                break;
            }
            flags = from[i++] | 0xFF00;
        }
        if (flags & 1) {
            unsigned char c;
            if (i >= packed) {
                break;
            }
            c = from[i++];
            if (o < size) {
                to[o++] = c;
            }
            ring[r] = c;
            r = (r + 1) & (RING - 1);
        } else {
            unsigned place, length;
            if (i + 1 >= packed) {
                break;
            }
            place = from[i] | ((from[i + 1] & 0xF0) << 4);
            length = (from[i + 1] & 0x0F) + SHORTEST;
            i += 2;
            for (k = 0; k <= length; k++) {
                unsigned char c = ring[(place + k) & (RING - 1)];
                if (o < size) {
                    to[o++] = c;
                }
                ring[r] = c;
                r = (r + 1) & (RING - 1);
            }
        }
    }
}

/* DOS's seek (INT 21h 42h) from the archive's start. */
static void seek(unsigned long offset)
{
    union REGS r;
    r.w.ax = 0x4200;
    r.w.bx = archive;
    r.w.cx = (unsigned)(offset >> 16);
    r.w.dx = (unsigned)offset;
    int86(0x21, &r, &r);
}

/* The member `name` decoded into `to` (`size` bytes at most): 0 if the archive hasn't it. */
int load(const char *name, unsigned char __far *to, unsigned size)
{
    unsigned i, got;
    unsigned char __far *packed;
    struct entry *e = 0;

    for (i = 0; i < entries; i++) {
        if (strncmp(directory[i].name, name, 8) == 0) {
            e = &directory[i];
            break;
        }
    }
    if (!e) {
        return 0;
    }
    packed = _fmalloc((unsigned)e->packed);
    if (!packed) {
        return 0;
    }
    seek(e->offset);
    _dos_read(archive, packed, (unsigned)e->packed, &got);
    switch (e->method) {
    case 1:
        unpack_runs(packed, got, to, size);
        break;
    case 2:
        unpack_lzss(packed, got, to, size);
        break;
    default:
        _fmemcpy(to, packed, got < size ? got : size);
        break;
    }
    _ffree(packed);
    loads++;
    return 1;
}

void load_level(void)
{
    char name[8] = "LEVEL0";
    unsigned i;
    name[5] = '0' + level;
    load(name, map, CELLS * CELLS);
    for (i = 0; i < CELLS * CELLS; i++) {
        if (map[i] == START) {
            player_x = i % CELLS;
            player_y = i / CELLS;
        }
    }
}

void wait_tick(void)
{
    while (timer_count == last_count) {
    }
}

void title_pass(void)
{
    last_count = timer_count;
    pass_count++;
    title_passes++;
    if (key_down[0x1C]) {
        level = 1;
        load_level();
        scene = 1;
    }
}

void play_pass(void)
{
    static const unsigned char scans[4] = { 0x4B, 0x4D, 0x48, 0x50 };
    static const signed char dx[4] = { -1, 1, 0, 0 };
    static const signed char dy[4] = { 0, 0, -1, 1 };
    unsigned char held = 0;
    int k;

    last_count = timer_count;
    pass_count++;
    for (k = 0; k < 4; k++) {
        unsigned char x, y, cell;
        if (!key_down[scans[k]]) {
            continue;
        }
        held |= 1 << k;
        if (keys_held & (1 << k)) {
            continue;
        }
        x = player_x + dx[k];
        y = player_y + dy[k];
        cell = map[y * CELLS + x];
        if (cell == WALL) {
            bumps++;
            continue;
        }
        player_x = x;
        player_y = y;
        moves++;
        if (cell == EXIT) {
            if (level < LEVELS) {
                level++;
                load_level();
            } else {
                scene = 2;
            }
            break;
        }
    }
    keys_held = held;
}

static void show_title(void)
{
    unsigned char __far *screen = MK_FP(0xA000, 0);
    union REGS r;
    unsigned i;

    r.w.ax = 0x13;
    int86(0x10, &r, &r);
    load("PALETTE", (unsigned char __far *)palette, sizeof palette);
    outp(0x3C8, 0);
    for (i = 0; i < sizeof palette; i++) {
        outp(0x3C9, palette[i]);
    }
    if (load("TITLE", picture, PICTURE)) {
        _fmemcpy(screen, picture, PICTURE);
    }
}

static unsigned char drawn_level, drawn_x, drawn_y;

static void draw_cell(unsigned x, unsigned y)
{
    static const unsigned char colours[4] = { 16, 40, 47, 16 };
    unsigned char __far *screen = MK_FP(0xA000, 0);
    unsigned char c = (x == player_x && y == player_y) ? 15 : colours[map[y * CELLS + x] & 3];
    unsigned i, j;
    for (j = 0; j < 6; j++) {
        for (i = 0; i < 6; i++) {
            screen[(y * 6 + j + 4) * 320 + x * 6 + i + 64] = c;
        }
    }
}

/* The level's cells when it changes, else the player's old and new cell. */
static void draw(void)
{
    unsigned x, y;
    if (drawn_level != level) {
        for (y = 0; y < CELLS; y++) {
            for (x = 0; x < CELLS; x++) {
                draw_cell(x, y);
            }
        }
        drawn_level = level;
    } else if (drawn_x != player_x || drawn_y != player_y) {
        draw_cell(drawn_x, drawn_y);
        draw_cell(player_x, player_y);
    }
    drawn_x = player_x;
    drawn_y = player_y;
}

static void set_divisor(unsigned divisor)
{
    outp(0x43, 0x36);
    outp(0x40, divisor & 0xFF);
    outp(0x40, divisor >> 8);
}

int main(void)
{
    char tag[4];
    unsigned got;
    union REGS r;

    if (_dos_open("VAULT.DAT", 0, &archive) != 0) {
        return 255;
    }
    _dos_read(archive, (void __far *)tag, 4, &got);
    _dos_read(archive, (void __far *)&entries, 2, &got);
    if (memcmp(tag, "VALT", 4) != 0 || entries > 8) {
        return 254;
    }
    _dos_read(archive, (void __far *)directory, entries * sizeof(struct entry), &got);
    map = _fmalloc(CELLS * CELLS);
    picture = _fmalloc(PICTURE);
    if (!map || !picture) {
        return 253;
    }

    old_timer = _dos_getvect(0x08);
    old_keyboard = _dos_getvect(0x09);
    _disable();
    _dos_setvect(0x08, on_timer);
    _dos_setvect(0x09, on_keyboard);
    set_divisor(17045); /* 1193182 / 70 */
    _enable();

    show_title();
    while (scene == 0 && !key_down[0x01]) {
        wait_tick();
        title_pass();
    }
    while (scene == 1 && !key_down[0x01]) {
        wait_tick();
        play_pass();
        draw();
    }

    _disable();
    set_divisor(0);
    _dos_setvect(0x08, old_timer);
    _dos_setvect(0x09, old_keyboard);
    _enable();
    r.w.ax = 0x03;
    int86(0x10, &r, &r);
    _dos_close(archive);
    return moves & 0xFF;
}
