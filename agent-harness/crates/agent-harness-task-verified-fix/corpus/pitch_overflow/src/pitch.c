/* Pitch-rate command from attitude error and controller gain. */
int pitch_cmd(int error, int gain) {
  int out = error * gain;
  __CPROVER_assert(out >= -1000 && out <= 1000,
                   "REQ-PITCH-1: pitch command within +/-1000");
  return out;
}
