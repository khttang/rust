int a[4];
int kinds(int i, int d, int s, int *p) {
  int x = a[i];
  int y = 10 / d;
  int z = 1 << s;
  int w = *p;
  unsigned u = (unsigned)i + 4000000000u;
  return x + y + z + w + (int)u;
}
