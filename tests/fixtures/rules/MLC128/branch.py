from manim import *


class MaybeInit(Scene):
    def __init__(self, fancy=False):
        if fancy:
            super().__init__()

    def construct(self):
        self.wait(1)


class Delegating(Scene):
    def __init__(self):
        self._boot()

    def _boot(self):
        self.section = 1

    def construct(self):
        self.wait(1)
