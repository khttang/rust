/* memory.x */
MEMORY
{
  /* Adjust ORIGIN and LENGTH to match your chip's datasheet specifications */
  FLASH : ORIGIN = 0x08000000, LENGTH = 512K
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
}
