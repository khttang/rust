int clamp(int x) {
  int r = x;
  if (r > 100) r = 100;
  if (r < -100) r = -100;
  __CPROVER_assert(r <= 100 && r >= -100, "result within limits");
  return r;
}
