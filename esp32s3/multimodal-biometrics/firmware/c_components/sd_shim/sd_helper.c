#include "driver/sdmmc_host.h"

sdmmc_host_t get_c_sdmmc_host_default(void) {
    sdmmc_host_t host = SDMMC_HOST_DEFAULT();
    return host;
}
