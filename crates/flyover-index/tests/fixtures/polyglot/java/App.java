package demo;

import java.util.List;
import java.util.Map;

public class App {
  public int add(int a, int b) {
    return a + b;
  }
}

interface Task {
  void run();
}

enum Status {
  OK,
  FAIL
}
