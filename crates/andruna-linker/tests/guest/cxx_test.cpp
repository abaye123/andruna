// LINK: -lc++
// C++ on real bionic and libc++: exceptions (unwinding found through
// dl_iterate_phdr and .eh_frame), RTTI, std::string, std::thread,
// thread_local, iostream.
#include <cstdio>
#include <iostream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

static int failures = 0;
#define CHECK(c)                                                   \
  do {                                                             \
    if (!(c)) {                                                    \
      std::printf("FAIL %s:%d %s\n", __FILE__, __LINE__, #c);      \
      failures++;                                                  \
    }                                                              \
  } while (0)

struct Tracker {
  int* p;
  explicit Tracker(int* q) : p(q) {}
  ~Tracker() { ++*p; }
};

__attribute__((noinline)) static void thrower(int depth) {
  if (depth == 0) throw std::runtime_error("deep");
  std::string s(100, 'x');
  thrower(depth - 1);
}

static thread_local std::string tl_name = "main";

int main() {
  int destroyed = 0;
  try {
    Tracker t(&destroyed);
    thrower(10);
  } catch (const std::runtime_error& e) {
    CHECK(std::string(e.what()) == "deep");
  }
  CHECK(destroyed == 1);
  try {
    throw 42;
  } catch (int v) {
    CHECK(v == 42);
  }
  std::vector<int> v;
  for (int i = 0; i < 1000; i++) v.push_back(i);
  CHECK(v[999] == 999);
  std::string other;
  std::thread th([&] {
    tl_name = "worker";
    try {
      throw std::logic_error("in thread");
    } catch (const std::exception& e) {
      other = e.what();
    }
  });
  th.join();
  CHECK(other == "in thread");
  CHECK(tl_name == "main");
  std::cout << "cxx_test " << (failures ? "FAILED" : "passed") << " (" << failures
            << " failures)" << std::endl;
  return failures ? 1 : 0;
}
