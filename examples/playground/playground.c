// playground.c — a friendly target for reclass-rs.
//
// A tiny "game" with a global Player struct (and a Weapon it points to) whose
// fields change a few times a second, so you can attach reclass-rs and watch
// memory update live: health oscillates, the position vector moves, the score
// ticks up, ammo counts down. Built non-PIE so the addresses it prints are
// stable across runs (great for following along with the guide).
//
//   make && ./playground
//
// offset, then loops forever. Point reclass-rs at &g_player and start typing.

#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

typedef struct Weapon {
    int32_t damage;   // +0x00
    int32_t ammo;     // +0x04
    char    name[16]; // +0x08
} Weapon;             // size 0x18

typedef struct Player {
    int32_t  health;      // +0x00  Int32
    int32_t  max_health;  // +0x04  Int32
    float    position[3]; // +0x08  Vec3 (x, y, z)
    uint32_t flags;       // +0x14  Hex32
    int32_t  alive;       // +0x18  Bool
    char     name[24];    // +0x1C  Text
    Weapon  *weapon;      // +0x38  Pointer / ClassPtr -> Weapon
    uint64_t score;       // +0x40  UInt64
} Player;                 // size 0x48

static Weapon g_weapon = {
    .damage = 42,
    .ammo = 30,
    .name = "Rifle",
};

static Player g_player = {
    .health = 100,
    .max_health = 100,
    .position = {1.0f, 2.0f, 3.0f},
    .flags = 0x00C0FFEE,
    .alive = 1,
    .name = "Player1",
    .weapon = &g_weapon,
    .score = 0,
};

int main(void) {
    printf("playground pid=%d\n", (int)getpid());
    printf("&g_player = %p\n", (void *)&g_player);
    printf("&g_weapon = %p\n", (void *)&g_weapon);
    printf("Player offsets: health=0x%02zx max_health=0x%02zx position=0x%02zx "
           "flags=0x%02zx alive=0x%02zx name=0x%02zx weapon=0x%02zx "
           "score=0x%02zx (sizeof=0x%zx)\n",
           offsetof(Player, health), offsetof(Player, max_health),
           offsetof(Player, position), offsetof(Player, flags),
           offsetof(Player, alive), offsetof(Player, name),
           offsetof(Player, weapon), offsetof(Player, score), sizeof(Player));
    printf("Weapon offsets: damage=0x%02zx ammo=0x%02zx name=0x%02zx "
           "(sizeof=0x%zx)\n",
           offsetof(Weapon, damage), offsetof(Weapon, ammo),
           offsetof(Weapon, name), sizeof(Weapon));
    fflush(stdout);

    double t = 0.0;
    for (;;) {
        g_player.health = 50 + (int32_t)(50.0 * sin(t)); // 0..100
        g_player.position[0] = (float)sin(t);
        g_player.position[1] = (float)cos(t);
        g_player.position[2] = (float)t;
        g_player.alive = g_player.health > 0;
        g_player.score += 7;
        g_weapon.ammo = (int32_t)(30 - ((long)t % 31));
        t += 0.1;
        usleep(100000); // ~10 Hz
    }
    return 0;
}
