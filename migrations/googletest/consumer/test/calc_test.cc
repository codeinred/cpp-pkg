// Exercises both gtest assertions and gmock matchers/mocks against the
// store-built googletest package; main() comes from GTest::gtest_main.
#include "gmock/gmock.h"
#include "gtest/gtest.h"

#include <vector>

int add(int a, int b) { return a + b; }

class Sink {
 public:
  virtual ~Sink() = default;
  virtual void put(int v) = 0;
};

class MockSink : public Sink {
 public:
  MOCK_METHOD(void, put, (int v), (override));
};

void drain(Sink& s, const std::vector<int>& vals) {
  for (int v : vals) s.put(v);
}

TEST(Calc, Adds) {
  EXPECT_EQ(add(2, 2), 4);
  EXPECT_THAT((std::vector<int>{add(1, 2), add(3, 4)}),
              testing::ElementsAre(3, 7));
}

TEST(Calc, DrainsThroughMock) {
  MockSink sink;
  testing::InSequence seq;
  EXPECT_CALL(sink, put(1));
  EXPECT_CALL(sink, put(2));
  drain(sink, {1, 2});
}
