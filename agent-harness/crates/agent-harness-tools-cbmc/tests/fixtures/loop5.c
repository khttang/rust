int count5(void) {
  int s = 0;
  for (int i = 0; i < 5; i++) s += 1;
  __CPROVER_assert(s == 5, "counted to five");
  return s;
}
