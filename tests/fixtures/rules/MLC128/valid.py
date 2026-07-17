from manim import *


class Good(Scene):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.section = 2

    def construct(self):
        self.wait(1)


class Legacy(Scene):
    def __init__(self):
        super(Legacy, self).__init__()

    def construct(self):
        self.wait(1)


class NoInit(Scene):
    def construct(self):
        self.wait(1)
