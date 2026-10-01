#include <limits.h>
#include <stdint.h>

/* Change between two consecutive 32-bit pressure samples. */
int32_t sensor_delta(int32_t previous, int32_t current) {
  int64_t delta = (int64_t)current - (int64_t)previous;
  if (delta > INT32_MAX)
    return INT32_MAX;
  if (delta < INT32_MIN)
    return INT32_MIN;
  return (int32_t)delta;
}
