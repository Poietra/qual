from manim import RED, Scene, Square


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        if square.submobjects:
            square.add_updater(lambda m, dt: m.set_fill(RED))
        self.wait(4)
