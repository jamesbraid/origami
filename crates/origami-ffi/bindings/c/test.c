/* Lists the fake machines, starts one, waits for a frame without polling,
 * takes it, and checks the wake handle goes quiet. Exits non-zero on the
 * first failure. Build and run with run.sh. */
#define _POSIX_C_SOURCE 200809L
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ORIGAMI_FAKE
#include "origami_frame.h"

#define CHECK(call)                                                           \
  do {                                                                        \
    origami_status st_ = (call);                                              \
    if (st_ != ORIGAMI_OK) {                                           \
      char msg_[256];                                                         \
      origami_last_error(msg_, sizeof msg_);                                  \
      fprintf(stderr, "%s:%d: %s -> %d: %s\n", __FILE__, __LINE__, #call,     \
              (int)st_, msg_);                                                \
      return 1;                                                               \
    }                                                                         \
  } while (0)

#define EXPECT(cond)                                                          \
  do {                                                                        \
    if (!(cond)) {                                                            \
      fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond);     \
      return 1;                                                               \
    }                                                                         \
  } while (0)

static volatile int wakes;
static void on_wake(void *user) { (void)user; wakes++; }

static int readable(ptrdiff_t fd) {
  struct pollfd p = {.fd = (int)fd, .events = POLLIN};
  return poll(&p, 1, 0) > 0;
}

int main(void) {
  char root[] = "/tmp/origami-c-test-XXXXXX";
  EXPECT(mkdtemp(root) != NULL);
  EXPECT(origami_frame_abi_version() == ORIGAMI_FRAME_ABI_VERSION);

  origami_core *core = NULL;
  CHECK(origami_core_new(root, &core));
  char key[512];
  CHECK(origami_fake_create_machine(core, "alpha", key, sizeof key));

  char list[2048];
  size_t needed = 0;
  CHECK(origami_core_machines(core, list, sizeof list, &needed));
  printf("machines:\n%s", list);
  EXPECT(strstr(list, "\talpha\n") != NULL);

  const origami_machine *machine = NULL;
  CHECK(origami_machine_open(core, key, &machine));
  CHECK(origami_machine_start(machine));

  origami_head_info head;
  size_t heads = 0;
  CHECK(origami_frame_heads(machine, &head, 1, &heads));
  EXPECT(heads == 1 && head.width == 64 && head.height == 48);

  ptrdiff_t fd = -1;
  CHECK(origami_frame_wake_handle(machine, &fd));
  CHECK(origami_frame_set_wake(machine, on_wake, NULL));
  EXPECT(!readable(fd) && wakes == 0); /* quiet until a frame is published */

  CHECK(origami_fake_advance_frame(core, key));
  EXPECT(readable(fd) && wakes == 1);
  CHECK(origami_frame_wake_clear(machine));
  EXPECT(!readable(fd));

  size_t cap = (size_t)head.width * head.height * 4;
  uint8_t *pixels = malloc(cap);
  origami_run runs[48];
  origami_frame frame = {.size = sizeof frame};
  CHECK(origami_frame_take(machine, 0, pixels, cap, runs, 48, &frame));
  EXPECT(frame.kind == ORIGAMI_FRAME_ROWS && frame.run_count == 1);
  EXPECT(runs[0].y0 == 0 && runs[0].y1 == 48 && frame.bytes_used == cap);
  /* pixel (x=3, y=2) is [seq, y, x, 0] */
  EXPECT(pixels[(2 * 64 + 3) * 4 + 1] == 2 && pixels[(2 * 64 + 3) * 4 + 2] == 3);
  CHECK(origami_frame_take(machine, 0, pixels, cap, runs, 48, &frame));
  EXPECT(frame.kind == ORIGAMI_FRAME_NONE);

  origami_input_event keys[2] = {
      {.kind = ORIGAMI_INPUT_KEY, .code = ORIGAMI_KEY_A, .down = 1},
      {.kind = ORIGAMI_INPUT_BUTTON, .code = ORIGAMI_BUTTON_LEFT, .down = 1}};
  CHECK(origami_input_send(machine, keys, 2));
  uint32_t released = 0;
  CHECK(origami_input_release_all(machine, &released));
  EXPECT(released == 2);

  origami_machine_release(machine);
  origami_core_release(core);
  free(pixels);
  printf("ok\n");
  return 0;
}
