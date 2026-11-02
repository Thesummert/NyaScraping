#include "types.h"

volatile int global_counter = 3;
static Sensor sensor = {5, "sensor"};
const int limit = 16;
int samples[4];

int compute(int input) {
    int local = input + global_counter;
    {
        int nested = local + sensor.value;
        samples[0] = nested;
    }
    return scale(local) + samples[0];
}

int main(void) {
    return compute(header_counter);
}
