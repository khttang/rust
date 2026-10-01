/* Pitch-rate command from attitude error and controller gain. */
int pitch_cmd(int error, int gain) {
  long long wide = (long long)error * gain;
  if (wide > 1000) wide = 1000;
  if (wide < -1000) wide = -1000;
  int out = (int)wide;
  __CPROVER_assert(out >= -1000 && out <= 1000,
                   "REQ-PITCH-1: pitch command within +/-1000");
  return out;
}
