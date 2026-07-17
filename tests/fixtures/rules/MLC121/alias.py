import manim as mn


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        self.add(square)
        square.add_updater(lambda m: self.wait(1))
