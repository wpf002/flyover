import os
from collections import OrderedDict


class Widget:
    def render(self):
        return "w"


def build():
    _ = os.getcwd()
    _ = OrderedDict()
    return Widget()
