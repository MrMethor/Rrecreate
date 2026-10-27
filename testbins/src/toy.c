/* A toy game to port: what Rrecreate's runtime and comparison are tested on (M3, M4).
 *
 * The timer runs at 70 Hz (INT 08h counts), the keyboard handler (INT 09h) keeps a key-state
 * array, and the main loop waits for the timer, then runs a pass: the player moves by the arrow
 * keys, Space drops a bomb that clears the enemies near the player, six enemies fall and bounce,
 * one that reaches the player costs a life, and a 32-bit random generator respawns them. Text mode
 * shows it. Esc, or the last life, ends the game; the exit code is the score's low byte.
 *
 * Positions are in 1/16 pixel of a 320x200 field. The game's state is in the named globals below,
 * so the linker map gives every item's address; and, as in real games, some of it is a pointer (the
 * nearest enemy), on the heap (the player's trail, through a far pointer) or packed bits (the
 * player's status). */
#include <conio.h>
#include <dos.h>
#include <i86.h>
#include <malloc.h>

#define ENEMIES 6
#define FIELD_W (320 * 16)
#define FIELD_H (200 * 16)
#define MAX_ELAPSED 4
#define PLAYER_SPEED 24
#define BOMB_RADIUS (40 * 16)
#define HIT_RANGE (8 * 16)
#define TRAIL 32

struct enemy {
    short x, y;
    short dx, dy;
    unsigned char alive;
    unsigned char hits;
};

volatile unsigned short timer_count;
volatile unsigned char key_down[128];

unsigned short last_count;
unsigned short elapsed;
unsigned short pass_count;
short player_x;
short player_y;
unsigned short score;
unsigned char lives;
unsigned char bombs;
unsigned char bomb_held;
unsigned char game_over;
unsigned long rng_seed;
struct enemy enemies[ENEMIES];
struct enemy *target;       /* the nearest enemy alive, or none */
short __far *trail;         /* on the heap: the player's x at the last TRAIL passes */
unsigned char status;       /* bit 0: moving; bits 1-2: facing (0 left, 1 right, 2 up, 3 down) */

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

unsigned short random(unsigned short range)
{
    rng_seed = rng_seed * 1103515245UL + 12345UL;
    return (unsigned short)(rng_seed >> 16) % range;
}

void spawn(struct enemy *e)
{
    e->x = random(320) * 16;
    e->y = random(100) * 16;
    e->dx = (short)random(9) - 4;
    if (e->dx == 0) {
        e->dx = 3;
    }
    e->dy = random(5) + 1;
    e->alive = 1;
    e->hits = 0;
}

void wait_tick(void)
{
    while (timer_count == last_count) {
    }
}

static short distance(short a, short b)
{
    return a > b ? a - b : b - a;
}

void update(void)
{
    int i;
    struct enemy *e;
    unsigned char moving = 0, facing = (status >> 1) & 3;
    long nearest = 0;

    elapsed = timer_count - last_count;
    if (elapsed > MAX_ELAPSED) {
        elapsed = MAX_ELAPSED;
    }
    last_count = timer_count;
    pass_count++;

    if (key_down[0x4B]) { player_x -= PLAYER_SPEED * elapsed; facing = 0; moving = 1; }
    if (key_down[0x4D]) { player_x += PLAYER_SPEED * elapsed; facing = 1; moving = 1; }
    if (key_down[0x48]) { player_y -= PLAYER_SPEED * elapsed; facing = 2; moving = 1; }
    if (key_down[0x50]) { player_y += PLAYER_SPEED * elapsed; facing = 3; moving = 1; }
    status = moving | (facing << 1);
    if (player_x < 0) player_x = 0;
    if (player_x > FIELD_W - 16) player_x = FIELD_W - 16;
    if (player_y < 0) player_y = 0;
    if (player_y > FIELD_H - 16) player_y = FIELD_H - 16;
    if (trail) {
        trail[pass_count % TRAIL] = player_x;
    }

    if (key_down[0x39] && !bomb_held && bombs > 0) {
        bombs--;
        for (i = 0; i < ENEMIES; i++) {
            long dx, dy;
            e = &enemies[i];
            dx = e->x - player_x;
            dy = e->y - player_y;
            if (e->alive && dx * dx + dy * dy < (long)BOMB_RADIUS * BOMB_RADIUS) {
                e->alive = 0;
                score += 10;
            }
        }
    }
    bomb_held = key_down[0x39];

    for (i = 0; i < ENEMIES; i++) {
        e = &enemies[i];
        if (!e->alive) {
            if (random(32) == 0) {
                spawn(e);
            }
            continue;
        }
        e->x += e->dx * (short)elapsed;
        e->y += e->dy * (short)elapsed;
        if (e->x < 0 || e->x > FIELD_W - 16) {
            e->dx = -e->dx;
            e->x += 2 * e->dx * (short)elapsed;
            e->hits++;
        }
        if (e->y > FIELD_H - 16) {
            e->y = 0;
            score++;
        }
        if (distance(e->x, player_x) < HIT_RANGE && distance(e->y, player_y) < HIT_RANGE) {
            e->alive = 0;
            if (--lives == 0) {
                game_over = 1;
            }
        }
    }

    target = 0;
    for (i = 0; i < ENEMIES; i++) {
        e = &enemies[i];
        if (e->alive) {
            long d = (long)distance(e->x, player_x) + distance(e->y, player_y);
            if (!target || d < nearest) {
                target = e;
                nearest = d;
            }
        }
    }
    score += elapsed;
}

static void put(int column, int row, char c, unsigned char colour)
{
    unsigned short __far *screen = MK_FP(0xB800, 0);
    if (column >= 0 && column < 80 && row >= 0 && row < 25) {
        screen[row * 80 + column] = (colour << 8) | (unsigned char)c;
    }
}

static void put_number(int column, unsigned short n)
{
    int i;
    for (i = 4; i >= 0; i--) {
        put(column + i, 0, '0' + n % 10, 0x0F);
        n /= 10;
    }
}

void draw(void)
{
    unsigned short __far *screen = MK_FP(0xB800, 0);
    int i;
    for (i = 0; i < 80 * 25; i++) {
        screen[i] = 0x0720;
    }
    put_number(0, score);
    put(8, 0, '0' + lives, 0x0C);
    put(10, 0, '0' + bombs, 0x0E);
    for (i = 0; i < ENEMIES; i++) {
        if (enemies[i].alive) {
            put(enemies[i].x / 64, enemies[i].y / 128, 'X', 0x0A);
        }
    }
    put(player_x / 64, player_y / 128, '@', 0x0F);
}

static void set_divisor(unsigned divisor)
{
    outp(0x43, 0x36);
    outp(0x40, divisor & 0xFF);
    outp(0x40, divisor >> 8);
}

int main(void)
{
    int i;

    player_x = FIELD_W / 2;
    player_y = FIELD_H - 32;
    lives = 3;
    bombs = 3;
    rng_seed = 20261026UL;
    for (i = 0; i < ENEMIES; i++) {
        spawn(&enemies[i]);
    }
    trail = _fmalloc(TRAIL * sizeof(short));
    if (trail) {
        for (i = 0; i < TRAIL; i++) {
            trail[i] = 0;
        }
    }

    old_timer = _dos_getvect(0x08);
    old_keyboard = _dos_getvect(0x09);
    _disable();
    _dos_setvect(0x08, on_timer);
    _dos_setvect(0x09, on_keyboard);
    set_divisor(17045); /* 1193182 / 70 */
    _enable();

    while (!game_over && !key_down[0x01]) {
        wait_tick();
        update();
        draw();
    }

    _disable();
    set_divisor(0);
    _dos_setvect(0x08, old_timer);
    _dos_setvect(0x09, old_keyboard);
    _enable();
    if (trail) {
        _ffree(trail);
    }
    return score & 0xFF;
}
