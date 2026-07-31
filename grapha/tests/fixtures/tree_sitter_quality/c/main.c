#include "helpers.h"

/* CWorker stores a callback and its label. */
typedef struct CWorker {
    void (*on_ready)(void);
    const char *label;
} CWorker;

typedef enum CStatus {
    C_STATUS_READY,
    C_STATUS_STOPPED,
} CStatus;

const char *format_label(const char *value) {
    return value;
}

static void report_ready(void) {}

static void run_worker(CWorker *worker) {
    const char *label = format_label("c");
    worker->on_ready();
    (void)label;
}

int main(void) {
    CWorker worker = {report_ready, format_label("c")};
    run_worker(&worker);
    return 0;
}
