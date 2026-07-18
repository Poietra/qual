from manim import RIGHT, ImageMobject, Scene, Square


class Demo(Scene):
    def construct(self):
        mover = Square()
        photo = ImageMobject("photo.png")
        self.add(mover, photo)
        self.play(mover.animate.shift(RIGHT))
