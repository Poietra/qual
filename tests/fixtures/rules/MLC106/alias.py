import manim as mn


class Demo(mn.Scene):
    def construct(self):
        self.wait(stop_condition=lambda: True, frozen_frame=True)
