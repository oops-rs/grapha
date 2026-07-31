#include "helpers.hpp"

#include <functional>
#include <string>

/** CppWorker demonstrates namespace-scoped declarations. */
namespace quality {

class BaseWorker {
public:
    virtual ~BaseWorker() = default;
};

class CppWorker : public BaseWorker {
public:
    std::function<void()> on_ready = [] { support::report_ready(); };

    void run() {
        on_ready();
    }

private:
    std::string label = support::format_label("cpp");
};

enum class CppStatus {
    Ready,
    Stopped,
};

} // namespace quality

int main() {
    quality::CppWorker worker;
    worker.run();
    return 0;
}
