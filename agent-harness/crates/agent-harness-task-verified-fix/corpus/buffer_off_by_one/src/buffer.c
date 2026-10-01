/* Telemetry frame buffer. */
int frame[8];

/* Zero every slot of the frame before reuse. */
void clear_frame(void) {
  for (int i = 0; i <= 8; i++)
    frame[i] = 0;
}
