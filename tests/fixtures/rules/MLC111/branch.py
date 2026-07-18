from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m, dt: m.rotate(dt))
        if config.frame_rate > 30:
            self.add(dot)
        self.wait(1)
