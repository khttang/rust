/* Scale a raw ADC count by a power of two. */
int scale(int raw, int shift) {
  if (shift < 0)
    shift = 0;
  if (shift > 15)
    shift = 15;
  long long wide = (long long)raw * (1LL << shift);
  if (wide > 2147483647LL)
    return 2147483647;
  if (wide < -2147483647LL - 1)
    return -2147483647 - 1;
  return (int)wide;
}
