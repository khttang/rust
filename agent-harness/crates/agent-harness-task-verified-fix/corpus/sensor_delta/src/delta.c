#include <limits.h>
#include <stdint.h>

/* Change between two consecutive 32-bit pressure samples. */
int32_t sensor_delta(int32_t previous, int32_t current) {
  return current - previous;
}
