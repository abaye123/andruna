// ART's metrics without statsd: the same functions ART's host build uses
// (art/runtime/metrics/statsd.h), compiled in place of statsd.cc, which
// needs the generated stats atoms and libstatssocket. Andruna has no statsd.

#include <memory>

#include "metrics/statsd.h"

namespace art HIDDEN {
namespace metrics {

std::unique_ptr<MetricsBackend> CreateStatsdBackend() { return nullptr; }

void SetupCallbackForDeviceStatus() {}

}  // namespace metrics
}  // namespace art
