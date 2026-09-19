#include <vector>
#include "widget.h"

namespace ui {

class Widget {
public:
  void draw();
};

void Widget::draw() {}

int square(int n) {
  return n * n;
}

}  // namespace ui
