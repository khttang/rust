/* Scale a raw ADC count by a power of two. */
int scale(int raw, int shift) {
  return raw << shift;
}
