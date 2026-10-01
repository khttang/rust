/* Mean of `count` sensor samples whose total is `sum`. */
int average(int sum, int count) {
  if (count <= 0)
    return 0;
  return sum / count;
}
