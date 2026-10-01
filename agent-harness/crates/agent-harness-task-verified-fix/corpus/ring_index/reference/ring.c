/* Four-slot ring buffer of air-data samples. */
int ring[4];

/* Store `value` at slot `head`; return the next slot. */
int ring_put(int head, int value) {
  int slot = head % 4;
  if (slot < 0)
    slot += 4;
  ring[slot] = value;
  return (slot + 1) % 4;
}
