typedef struct Sensor {
    int value;
    const char *label;
} Sensor;

static volatile int header_counter = 7;

static inline int scale(int input) {
    int scaled = input * 2;
    header_counter += scaled;
    return scaled;
}
