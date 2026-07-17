from manim import *


class Demo(Scene):
    def construct(self):
        def callback(m):
            self.wait(1)

        square = Square()
        self.add(square)
        square.add_updater(callback)
