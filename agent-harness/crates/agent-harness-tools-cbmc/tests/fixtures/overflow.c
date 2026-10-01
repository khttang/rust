int pitch_cmd(int error, int gain) {
  int out = error * gain;
  return out;
}
